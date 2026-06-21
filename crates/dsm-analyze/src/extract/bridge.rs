use crate::extract::*;
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// Detect cross-language bridges that aren't captured by individual extractors.
/// These include shared schema files (protobuf, thrift, avro), docker-compose
/// service dependencies, and build script orchestration.
pub fn detect_cross_language_bridges(
    dir: &Path,
    per_language_edges: &HashMap<Language, Vec<Edge>>,
) -> Vec<Edge> {
    let mut edges = Vec::new();

    // Scan for shared protobuf/thrift/avro schemas
    if let Ok(proto_edges) = scan_proto_schemas(dir, per_language_edges) {
        edges.extend(proto_edges);
    }

    // Scan docker-compose for service relationships
    if let Ok(docker_edges) = scan_docker_compose(dir) {
        edges.extend(docker_edges);
    }

    // Scan for shared header files used by multiple languages
    if let Ok(header_edges) = scan_shared_headers(dir, per_language_edges) {
        edges.extend(header_edges);
    }

    edges
}

fn scan_proto_schemas(
    dir: &Path,
    per_language_edges: &HashMap<Language, Vec<Edge>>,
) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    let languages_present: Vec<&Language> = per_language_edges.keys().collect();

    // Find all .proto files
    let proto_files = find_files_recursive(dir, "proto");
    let service_re = regex::Regex::new(r"service\s+(\w+)")?;

    for proto_file in &proto_files {
        let content = std::fs::read_to_string(proto_file)?;

        for cap in service_re.captures_iter(&content) {
            let service_name = &cap[1];

            // Check which languages have generated stubs for this service
            let mut langs_with_stubs = Vec::new();

            // Java: look for *Grpc.java
            if languages_present.contains(&&Language::Java) {
                let java_stub = format!("{}Grpc.java", service_name);
                if file_exists_recursive(dir, &java_stub) {
                    langs_with_stubs.push(Language::Java);
                }
            }

            // Python: look for *_pb2_grpc.py
            if languages_present.contains(&&Language::Python) {
                let py_stub = format!("{}_pb2_grpc.py", to_snake_case(service_name));
                if file_exists_recursive(dir, &py_stub) {
                    langs_with_stubs.push(Language::Python);
                }
            }

            // Go: look for *_grpc.pb.go
            if languages_present.contains(&&Language::Go) {
                let go_stub = format!("{}_grpc.pb.go", to_snake_case(service_name));
                if file_exists_recursive(dir, &go_stub) {
                    langs_with_stubs.push(Language::Go);
                }
            }

            // TypeScript: look for *_grpc_pb.js or *_grpc_pb.ts
            if languages_present.contains(&&Language::TypeScript) {
                let ts_stub = format!("{}_grpc_pb", to_snake_case(service_name));
                if file_exists_recursive(dir, &format!("{}.ts", ts_stub))
                    || file_exists_recursive(dir, &format!("{}.js", ts_stub))
                {
                    langs_with_stubs.push(Language::TypeScript);
                }
            }

            // Create edges between all pairs of languages with stubs
            for i in 0..langs_with_stubs.len() {
                for j in (i + 1)..langs_with_stubs.len() {
                    edges.push(Edge {
                        source: format!("{}:{}", langs_with_stubs[i], service_name),
                        target: format!("{}:{}", langs_with_stubs[j], service_name),
                        weight: 1.0,
                        kind: EdgeKind::Ipc,
                        cross_language: Some(CrossLanguageEdge {
                            source_lang: langs_with_stubs[i].clone(),
                            target_lang: langs_with_stubs[j].clone(),
                            mechanism: FfiMechanism::Grpc,
                        }),
                    });
                }
            }
        }
    }

    // Also check for thrift schemas
    let thrift_files = find_files_recursive(dir, "thrift");
    for thrift_file in &thrift_files {
        let content = std::fs::read_to_string(thrift_file)?;

        for cap in service_re.captures_iter(&content) {
            let service_name = &cap[1];
            // Create a generic shared-proto edge
            if languages_present.len() >= 2 {
                edges.push(Edge {
                    source: format!("thrift:{}", service_name),
                    target: format!("thrift:{}:consumers", service_name),
                    weight: 1.0,
                    kind: EdgeKind::Ipc,
                    cross_language: Some(CrossLanguageEdge {
                        source_lang: languages_present[0].clone(),
                        target_lang: languages_present[1].clone(),
                        mechanism: FfiMechanism::SharedProto,
                    }),
                });
            }
        }
    }

    Ok(edges)
}

fn scan_docker_compose(dir: &Path) -> Result<Vec<Edge>> {
    let mut edges = Vec::new();
    // Simple YAML parsing for depends_on
    let service_re = regex::Regex::new(r"(?m)^  (\w[\w-]*):")?;
    let dep_item_re = regex::Regex::new(r"(?m)^\s+-\s+(\w[\w-]*)")?;

    for name in &[
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let compose_file = dir.join(name);
        if compose_file.exists() {
            let content = std::fs::read_to_string(&compose_file)?;

            let _services: Vec<String> = service_re
                .captures_iter(&content)
                .map(|c| c[1].to_string())
                .collect();

            // Parse depends_on relationships
            let mut current_service = String::new();

            for line in content.lines() {
                let trimmed = line.trim();
                // Check if this is a top-level service definition
                if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.ends_with(':') {
                    current_service = trimmed.trim_end_matches(':').to_string();
                }
                if line.contains("depends_on:") {
                    // Next lines with "- servicename" are dependencies
                    continue;
                }
                if let Some(cap) = dep_item_re.captures(line) {
                    if !current_service.is_empty() {
                        edges.push(Edge {
                            source: format!("docker:{}", current_service),
                            target: format!("docker:{}", &cap[1]),
                            weight: 1.0,
                            kind: EdgeKind::Ipc,
                            cross_language: Some(CrossLanguageEdge {
                                source_lang: Language::Unknown("docker".to_string()),
                                target_lang: Language::Unknown("docker".to_string()),
                                mechanism: FfiMechanism::Rest,
                            }),
                        });
                    }
                }
            }

            break; // Only process first compose file found
        }
    }

    Ok(edges)
}

fn scan_shared_headers(
    dir: &Path,
    per_language_edges: &HashMap<Language, Vec<Edge>>,
) -> Result<Vec<Edge>> {
    // Look for .h files that might be shared between C, Rust (via bindgen), Python (via cffi), etc.
    let header_files = find_files_recursive(dir, "h");
    let mut edges = Vec::new();

    let languages_present: Vec<&Language> = per_language_edges.keys().collect();

    // If we have C/C++ headers and multiple languages, check for JNI headers
    for header in &header_files {
        let name = header.file_name().unwrap_or_default().to_string_lossy();

        // JNI headers follow pattern: Java_package_Class_method
        if (name.starts_with("Java_") || name.contains("_jni"))
            && languages_present.contains(&&Language::Java)
        {
            edges.push(Edge {
                source: format!("c:{}", name),
                target: "java:native_methods".to_string(),
                weight: 1.0,
                kind: EdgeKind::Ffi,
                cross_language: Some(CrossLanguageEdge {
                    source_lang: Language::C,
                    target_lang: Language::Java,
                    mechanism: FfiMechanism::Jni,
                }),
            });
        }
    }

    Ok(edges)
}

/// Find all files with a given extension recursively.
fn find_files_recursive(dir: &Path, ext: &str) -> Vec<std::path::PathBuf> {
    super::java::walkdir(dir, ext)
}

/// Check if a file with the given name exists anywhere under dir.
fn file_exists_recursive(dir: &Path, filename: &str) -> bool {
    fn check_inner(dir: &Path, filename: &str) -> bool {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    if !name.starts_with('.')
                        && name != "node_modules"
                        && name != "target"
                        && check_inner(&path, filename)
                    {
                        return true;
                    }
                } else if path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy() == filename)
                {
                    return true;
                }
            }
        }
        false
    }
    check_inner(dir, filename)
}

/// Convert CamelCase to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.push(ch.to_lowercase().next().unwrap_or(ch));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_snake_case_works() {
        assert_eq!(to_snake_case("UserService"), "user_service");
        assert_eq!(to_snake_case("HTTPClient"), "h_t_t_p_client");
        assert_eq!(to_snake_case("simple"), "simple");
    }

    #[test]
    fn empty_bridges() {
        let edges = detect_cross_language_bridges(Path::new("/nonexistent"), &HashMap::new());
        assert!(edges.is_empty());
    }
}
