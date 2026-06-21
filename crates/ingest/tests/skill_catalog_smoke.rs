//! Smoke test: walk + parse a real SKILL.md catalog.
//!
//! Marked `#[ignore]` so it does not run in CI by default. Point
//! `FORGE_SKILL_CATALOG_DIR` at a local skill catalog and run with:
//!
//!     cargo test -p forge-ingest --test skill_catalog_smoke -- --ignored --nocapture

use forge_ingest::skill_ingest::{build_args, parse, walk};
use std::{env, path::PathBuf};

#[test]
#[ignore]
fn walk_and_parse_skill_catalog() {
    let Some(root) = env::var_os("FORGE_SKILL_CATALOG_DIR").map(PathBuf::from) else {
        eprintln!("FORGE_SKILL_CATALOG_DIR not set; skipping");
        return;
    };
    if !root.exists() {
        eprintln!("catalog not found at {}; skipping", root.display());
        return;
    }
    let files = walk::walk(&root).expect("walk");
    println!("found {} SKILL.md files", files.len());

    let mut ok = 0usize;
    let mut err = 0usize;
    let mut empty_steps = 0usize;
    let mut with_tags = 0usize;
    for f in &files {
        match parse::parse(&f.bytes, &f.category) {
            Ok(s) => {
                ok += 1;
                if s.steps_empty {
                    empty_steps += 1;
                }
                if !s.tags.is_empty() {
                    with_tags += 1;
                }
                // Adapter smoke: every parsed skill must produce valid
                // IngestSkillArgs. Content hash is opaque; serialize to
                // JSON as a byte-level sanity check.
                let args = build_args::build_ingest_args(&s, &[], None);
                let wire = serde_json::to_value(&args).expect("args serialize");
                assert_eq!(wire["name"], s.name);
                assert!(wire["content_hash"]
                    .as_str()
                    .unwrap()
                    .starts_with("sha256:"));
            }
            Err(e) => {
                err += 1;
                println!("  PARSE ERR {}: {}", f.path.display(), e);
            }
        }
    }
    println!("parsed ok={ok} err={err} empty_steps={empty_steps} with_tags={with_tags}");
    assert!(ok > 0, "no skills parsed");
}
