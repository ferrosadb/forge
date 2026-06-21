//! Single-read file buffer: read a source file once, derive sha256, line count,
//! and optional text body.  All downstream consumers (file entity, LSP didOpen,
//! symbol range slicing) share the same `SourceBuffer` — never re-read the file.
//!
//! Hazard guards: P1-3 (one read), P1-4 (size cap), P1-5 (strict UTF-8),
//!                R-P1-12 (streaming sha256), F7 (oversize), F14 (editor-save race),
//!                F20 (sha256 for change detection).

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::extractor::MAX_FILE_BYTES;

/// Chunk size for streaming hash.  64 KiB balances syscall count vs stack/heap pressure.
const HASH_CHUNK: usize = 64 * 1024;

/// A pre-read source file with derived statistics.
///
/// `text` is `None` when the file exceeds `MAX_FILE_BYTES`; all other fields
/// are populated regardless of file size.
#[derive(Debug)]
pub struct SourceBuffer {
    /// Absolute path to the source file.
    pub path: PathBuf,
    /// File size in bytes (from `stat`, before any read).
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the raw file bytes.
    pub sha256: String,
    /// Full file text, or `None` if the file exceeds `MAX_FILE_BYTES`.
    pub text: Option<String>,
    /// `true` iff `bytes > MAX_FILE_BYTES`.
    pub truncated: bool,
    /// Number of newline-terminated lines (1 for a file with no newlines).
    pub lines: u32,
}

impl SourceBuffer {
    /// Read a source file into a `SourceBuffer`.
    ///
    /// For files within `MAX_FILE_BYTES`:
    ///   1. `stat` to get byte count.
    ///   2. Read bytes into memory.
    ///   3. Validate strict UTF-8 (non-UTF-8 → `Err`; caller skips the file).
    ///   4. Hash the bytes.
    ///   5. Count lines.
    ///
    /// For oversized files:
    ///   - Stream-hash via 64 KiB chunks; `text = None`, `truncated = true`.
    ///   - `lines` is set to 0 (unknown without reading the full body).
    ///
    /// Fails loud on any I/O error (with path context).
    pub fn read(path: &Path) -> Result<Self> {
        // Step 1: stat for byte count.
        let meta =
            std::fs::metadata(path).with_context(|| format!("stat failed: {}", path.display()))?;
        let bytes = meta.len();

        if bytes > MAX_FILE_BYTES {
            // Oversized: stream-hash only, no text body.
            let sha256 = hash_file_streaming(path)?;
            return Ok(Self {
                path: path.to_path_buf(),
                bytes,
                sha256,
                text: None,
                truncated: true,
                lines: 0,
            });
        }

        // Step 2: read all bytes.
        let raw =
            std::fs::read(path).with_context(|| format!("read failed: {}", path.display()))?;

        // Step 3: validate strict UTF-8.
        let text = std::str::from_utf8(&raw)
            .with_context(|| format!("non-UTF-8 source file: {}", path.display()))?
            .to_string();

        // Step 4: hash the bytes (after UTF-8 validation, raw bytes are canonical).
        let sha256 = hex_sha256(&raw);

        // Step 5: count lines.
        let lines = count_lines(&text);

        Ok(Self {
            path: path.to_path_buf(),
            bytes,
            sha256,
            text: Some(text),
            truncated: false,
            lines,
        })
    }
}

/// Compute the SHA-256 of `data` and return a lowercase hex string.
fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Stream-hash a file in `HASH_CHUNK`-byte chunks; return lowercase hex SHA-256.
///
/// Used for files exceeding `MAX_FILE_BYTES` so we never load the full body into RAM.
/// Hazard guard: R-P1-12 (streaming hash, bounded allocation).
fn hash_file_streaming(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("open for hashing failed: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(HASH_CHUNK, file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK];

    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("read during hash: {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Count the number of lines in `text`.
///
/// Returns at least 1 for any non-empty string without a newline.
/// Returns 0 for an empty string.
///
/// Power of 10 Rule 2: bounded iteration — iterates exactly `text.len()` characters.
fn count_lines(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    // Each '\n' terminates a line; the final line may lack a terminator.
    let newlines = text.bytes().filter(|&b| b == b'\n').count();
    // If the text ends with '\n', the count equals the number of lines.
    // If not, there is one additional unterminated line.
    let trailing_newline = text.ends_with('\n');
    let count = if trailing_newline {
        newlines
    } else {
        newlines + 1
    };
    // Clamp to u32; files > 4 B lines are not a concern (MAX_FILE_BYTES ≪ u32::MAX).
    u32::try_from(count).unwrap_or(u32::MAX)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

    use super::*;

    /// T1 verification: `source_buffer::small_file_has_text`
    /// A file under MAX_FILE_BYTES should have truncated=false and text=Some.
    #[test]
    fn small_file_has_text() {
        let mut f = NamedTempFile::new().unwrap();
        let content = "hello world\n";
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();

        let buf = SourceBuffer::read(f.path()).expect("read should succeed");
        assert!(!buf.truncated);
        assert_eq!(buf.text.as_deref(), Some("hello world\n"));
        assert_eq!(buf.bytes, content.len() as u64);
        assert_eq!(buf.lines, 1);
    }

    /// T1 verification: `source_buffer::oversize_file_no_text`
    /// A file > MAX_FILE_BYTES should have truncated=true, text=None, but sha256 populated.
    #[test]
    fn oversize_file_no_text() {
        let mut f = NamedTempFile::new().unwrap();
        // Write MAX_FILE_BYTES + 1 bytes
        let chunk = vec![b'a'; 1024];
        let chunks_needed = (MAX_FILE_BYTES / 1024 + 2) as usize;
        for _ in 0..chunks_needed {
            f.write_all(&chunk).unwrap();
        }
        f.flush().unwrap();

        let buf = SourceBuffer::read(f.path()).expect("read should succeed");
        assert!(buf.truncated);
        assert!(buf.text.is_none());
        assert!(buf.bytes > MAX_FILE_BYTES);
        // sha256 must be populated even for oversized files
        assert_eq!(
            buf.sha256.len(),
            64,
            "sha256 should be a 64-char hex string"
        );
    }

    /// T1 verification: `source_buffer::sha256_matches_known_vector`
    /// SHA-256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
    #[test]
    fn sha256_matches_known_vector() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello").unwrap();
        f.flush().unwrap();

        let buf = SourceBuffer::read(f.path()).expect("read should succeed");
        assert_eq!(
            buf.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    /// Verify that non-UTF-8 files are rejected with a clear error (not panicked).
    #[test]
    fn non_utf8_file_returns_error() {
        let mut f = NamedTempFile::new().unwrap();
        // 0xFF is not valid UTF-8
        f.write_all(&[0xFF, 0xFE, 0x00]).unwrap();
        f.flush().unwrap();

        let result = SourceBuffer::read(f.path());
        assert!(result.is_err(), "non-UTF-8 file must return Err, not panic");
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("non-UTF-8"),
            "error message should mention non-UTF-8: {msg}"
        );
    }

    #[test]
    fn count_lines_empty() {
        assert_eq!(count_lines(""), 0);
    }

    #[test]
    fn count_lines_no_trailing_newline() {
        assert_eq!(count_lines("abc"), 1);
    }

    #[test]
    fn count_lines_with_trailing_newline() {
        assert_eq!(count_lines("abc\n"), 1);
    }

    #[test]
    fn count_lines_multiple() {
        assert_eq!(count_lines("a\nb\nc\n"), 3);
        assert_eq!(count_lines("a\nb\nc"), 3);
    }
}
