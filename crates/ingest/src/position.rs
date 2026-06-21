//! UTF-8 / UTF-16 / UTF-32 position encoding helpers for LSP range conversion.
//!
//! The LSP specification (3.17) defaults to UTF-16 code-unit positions.
//! This module converts LSP `(line, character)` pairs to byte offsets,
//! supporting all three position encoding kinds negotiated during `initialize`.
//!
//! Hazard guards:
//!   - F2  (RPN 432): UTF-16 code-unit mismatch on non-ASCII identifiers.
//!   - P0-1: negotiate positionEncodings in initialize; convert correctly.
//!   - P0-2: never index `str` directly — always use `str::get`.

/// The position encoding negotiated with the LSP server.
///
/// The LSP 3.17 spec defines three encodings; the server picks one from
/// the client-advertised list and reports it in `initializeResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    /// `character` is a byte offset within the line.
    Utf8,
    /// `character` counts UTF-16 code units (the LSP default when not negotiated).
    Utf16,
    /// `character` is a Unicode scalar value (code point) count.
    Utf32,
}

impl PositionEncoding {
    /// Parse the `positionEncoding` string returned by the LSP server.
    ///
    /// Returns `Utf16` on any unrecognised value, matching the LSP default.
    pub fn from_lsp_str(s: &str) -> Self {
        match s {
            "utf-8" | "utf8" => Self::Utf8,
            "utf-32" | "utf32" => Self::Utf32,
            // "utf-16" | "utf16" and anything unknown → default
            _ => Self::Utf16,
        }
    }
}

/// A pre-built index from line numbers to byte offsets within a source string.
///
/// `new` is O(n) in the file size; all `pos_to_byte` calls are then O(line_length).
pub struct LineIndex {
    /// `line_starts[i]` is the byte offset of the first character on line `i` (0-indexed).
    line_starts: Vec<usize>,
    /// A copy of the source text, kept for character iteration within a line.
    source: String,
}

impl LineIndex {
    /// Build a `LineIndex` from source text.
    ///
    /// Power of 10 Rule 2: this loop is bounded by `source.len()`.
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                // Next line starts at the byte after this newline.
                line_starts.push(i + 1);
            }
        }
        Self {
            line_starts,
            source: source.to_string(),
        }
    }

    /// Convert an LSP `(line, character)` position to a byte offset.
    ///
    /// Returns `None` if:
    ///   - `line` is out of bounds.
    ///   - `character` refers past the end of the line.
    ///   - The computed byte offset is not on a UTF-8 character boundary.
    ///     (Catches the half-surrogate-pair hazard: a UTF-16 `character`
    ///     of 1 into a 4-byte emoji is not a valid byte boundary.)
    ///
    /// Callers must treat `None` as a skip condition, not a panic (P0-2).
    pub fn pos_to_byte(
        &self,
        line: u32,
        character: u32,
        encoding: PositionEncoding,
    ) -> Option<usize> {
        let line = usize::try_from(line).ok()?;
        let char_idx = usize::try_from(character).ok()?;

        let line_start = *self.line_starts.get(line)?;
        // End of this line = start of next line (or end of source).
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());

        let line_text = self.source.get(line_start..line_end)?;

        let byte_offset_within_line = match encoding {
            PositionEncoding::Utf8 => {
                // `character` is a byte offset; validate it falls on a char boundary.
                if char_idx > line_text.len() {
                    return None;
                }
                char_idx
            }
            PositionEncoding::Utf16 => {
                // Walk chars accumulating UTF-16 code units until we reach `char_idx`.
                // Power of 10 Rule 2: bounded by the line length (≤ MAX_FILE_BYTES).
                let mut utf16_count = 0usize;
                let mut byte_pos = 0usize;
                for ch in line_text.chars() {
                    if utf16_count == char_idx {
                        break;
                    }
                    utf16_count += ch.len_utf16();
                    byte_pos += ch.len_utf8();
                    if utf16_count > char_idx {
                        // `char_idx` landed inside a surrogate pair — not a valid boundary.
                        // Example: character=1 into a 4-byte emoji (2 UTF-16 units) is invalid.
                        return None;
                    }
                }
                if utf16_count < char_idx {
                    // `char_idx` is past the end of the line.
                    return None;
                }
                byte_pos
            }
            PositionEncoding::Utf32 => {
                // Walk chars counting code points until we reach `char_idx`.
                // Power of 10 Rule 2: bounded by the line length.
                let mut cp_count = 0usize;
                let mut byte_pos = 0usize;
                for ch in line_text.chars() {
                    if cp_count == char_idx {
                        break;
                    }
                    cp_count += 1;
                    byte_pos += ch.len_utf8();
                }
                if cp_count < char_idx {
                    // `char_idx` is past the end of the line.
                    return None;
                }
                byte_pos
            }
        };

        let result = line_start + byte_offset_within_line;

        // Invariant: result must be on a UTF-8 character boundary (P0-2).
        if !self.source.is_char_boundary(result) {
            return None;
        }

        Some(result)
    }
}

/// Safely slice `source[start..end]`.
///
/// Returns `None` if either bound is out of range or not on a character boundary.
/// This is the **only** safe way to slice source text in this codebase — direct
/// `source[a..b]` indexing is forbidden (hazard P0-2 / `indexing_slicing = deny`).
///
/// On `None`, callers must log and skip the symbol; they must never panic.
pub fn slice_source(source: &str, start: usize, end: usize) -> Option<&str> {
    source.get(start..end)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── UTF-8 encoding ─────────────────────────────────────────────────────────

    /// T1 verification: `position::utf8_simple_ascii`
    /// Line 0, character 5 of an ASCII line → byte offset 5.
    #[test]
    fn utf8_simple_ascii() {
        let source = "hello world\n";
        let idx = LineIndex::new(source);
        let result = idx.pos_to_byte(0, 5, PositionEncoding::Utf8);
        assert_eq!(result, Some(5));
    }

    // ── UTF-16 encoding ────────────────────────────────────────────────────────

    /// T1 verification: `position::utf16_emoji_shifts_byte`
    ///
    /// Line = "😀x\n"
    ///   - byte layout:  [0xF0, 0x9F, 0x98, 0x80] [0x78] [0x0A]
    ///   - UTF-16 units: 😀 = 2 units, x = 1 unit
    ///
    /// UTF-16 character 2 (after the emoji) should resolve to byte 4.
    #[test]
    fn utf16_emoji_shifts_byte() {
        let source = "😀x\n";
        let idx = LineIndex::new(source);
        // character 0 → byte 0 (start of emoji)
        assert_eq!(idx.pos_to_byte(0, 0, PositionEncoding::Utf16), Some(0));
        // character 2 → byte 4 (after the emoji, at 'x')
        assert_eq!(idx.pos_to_byte(0, 2, PositionEncoding::Utf16), Some(4));
        // character 3 → byte 5 (at '\n')
        assert_eq!(idx.pos_to_byte(0, 3, PositionEncoding::Utf16), Some(5));
    }

    /// T1 verification: `position::utf32_emoji`
    ///
    /// Line = "😀x\n"
    /// UTF-32 character 1 (second code point, 'x') should resolve to byte 4.
    #[test]
    fn utf32_emoji() {
        let source = "😀x\n";
        let idx = LineIndex::new(source);
        // character 0 → byte 0 (start of emoji)
        assert_eq!(idx.pos_to_byte(0, 0, PositionEncoding::Utf32), Some(0));
        // character 1 → byte 4 ('x', the second code point)
        assert_eq!(idx.pos_to_byte(0, 1, PositionEncoding::Utf32), Some(4));
    }

    /// T1 verification: `position::out_of_bounds_returns_none`
    /// Line/character past end of source → None.
    #[test]
    fn out_of_bounds_returns_none() {
        let source = "hi\n";
        let idx = LineIndex::new(source);
        // Line 99 doesn't exist
        assert_eq!(idx.pos_to_byte(99, 0, PositionEncoding::Utf8), None);
        // character 100 on line 0 ("hi\n" has only 3 bytes)
        assert_eq!(idx.pos_to_byte(0, 100, PositionEncoding::Utf8), None);
        assert_eq!(idx.pos_to_byte(0, 100, PositionEncoding::Utf16), None);
        assert_eq!(idx.pos_to_byte(0, 100, PositionEncoding::Utf32), None);
    }

    /// T1 verification: `position::char_boundary_violation_returns_none`
    ///
    /// UTF-16 character 1 into "😀\n" means one UTF-16 code unit into a
    /// surrogate pair — this is NOT a valid byte boundary and must return None.
    #[test]
    fn char_boundary_violation_returns_none() {
        // "😀" is U+1F600: encoded as 4 bytes in UTF-8, 2 code units in UTF-16.
        // UTF-16 character 1 = after one code unit = halfway through the emoji.
        // Byte 2 is not a char boundary → must return None.
        let source = "😀\n";
        let idx = LineIndex::new(source);
        // character 1 = one UTF-16 unit in = inside the surrogate pair
        assert_eq!(
            idx.pos_to_byte(0, 1, PositionEncoding::Utf16),
            None,
            "UTF-16 char 1 into a 4-byte emoji must return None (not a boundary)"
        );
        // Sanity: character 0 is valid
        assert_eq!(idx.pos_to_byte(0, 0, PositionEncoding::Utf16), Some(0));
        // Sanity: character 2 (after the emoji) is valid → byte 4
        assert_eq!(idx.pos_to_byte(0, 2, PositionEncoding::Utf16), Some(4));
    }

    // ── slice_source ───────────────────────────────────────────────────────────

    #[test]
    fn slice_source_valid() {
        assert_eq!(slice_source("hello world", 0, 5), Some("hello"));
        assert_eq!(slice_source("hello", 5, 5), Some(""));
    }

    #[test]
    fn slice_source_out_of_range_returns_none() {
        assert_eq!(slice_source("hi", 0, 100), None);
    }

    #[test]
    fn slice_source_non_boundary_returns_none() {
        // byte 1 of a 4-byte emoji is not a char boundary
        let s = "😀";
        assert_eq!(slice_source(s, 0, 1), None);
        assert_eq!(slice_source(s, 0, 4), Some("😀")); // full emoji is valid
    }

    // ── PositionEncoding::from_lsp_str ─────────────────────────────────────────

    #[test]
    fn from_lsp_str_parses_all_variants() {
        assert_eq!(
            PositionEncoding::from_lsp_str("utf-8"),
            PositionEncoding::Utf8
        );
        assert_eq!(
            PositionEncoding::from_lsp_str("utf8"),
            PositionEncoding::Utf8
        );
        assert_eq!(
            PositionEncoding::from_lsp_str("utf-16"),
            PositionEncoding::Utf16
        );
        assert_eq!(
            PositionEncoding::from_lsp_str("utf-32"),
            PositionEncoding::Utf32
        );
        assert_eq!(
            PositionEncoding::from_lsp_str("utf32"),
            PositionEncoding::Utf32
        );
        // Unknown → default UTF-16
        assert_eq!(
            PositionEncoding::from_lsp_str("unknown"),
            PositionEncoding::Utf16
        );
        assert_eq!(PositionEncoding::from_lsp_str(""), PositionEncoding::Utf16);
    }
}
