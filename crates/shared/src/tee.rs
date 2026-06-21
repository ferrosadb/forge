//! Tee/fallback: save raw output on command failure for debugging.
//!
//! When a command fails (non-zero exit), the unfiltered output is saved
//! to ~/.local/share/forge/tee/ so the LLM can read the full output
//! if needed, without it consuming context by default.

use anyhow::Result;
use std::path::PathBuf;

/// Get the tee output directory.
pub fn tee_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("forge")
        .join("tee");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Save raw output to a timestamped file. Returns the file path.
pub fn save_raw_output(command: &str, output: &str) -> Result<PathBuf> {
    let dir = tee_dir()?;

    // Sanitize command for filename
    let safe_cmd: String = command
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(50)
        .collect();

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("{}_{}.log", timestamp, safe_cmd);
    let path = dir.join(&filename);

    std::fs::write(&path, output)?;

    // Rotate: keep only the most recent max_files
    rotate_tee_files(&dir, 20)?;

    Ok(path)
}

/// Keep only the N most recent files in the tee directory.
fn rotate_tee_files(dir: &std::path::Path, max_files: usize) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "log"))
        .collect();

    if entries.len() <= max_files {
        return Ok(());
    }

    entries.sort_by_key(|e| e.file_name());
    let to_remove = entries.len() - max_files;
    for entry in entries.into_iter().take(to_remove) {
        let _ = std::fs::remove_file(entry.path());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_save_and_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let _path = dir.path().join("test.log");

        // Just test the rotation logic
        for i in 0..5 {
            let p = dir.path().join(format!("test_{}.log", i));
            fs::write(&p, format!("content {}", i)).unwrap();
        }

        rotate_tee_files(dir.path(), 3).unwrap();

        let remaining: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(remaining.len(), 3);
    }
}
