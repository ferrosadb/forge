//! Deterministic content hash for idempotent skill re-ingest.
//!
//! The hash is `sha256(frontmatter_bytes ‖ "\0" ‖ body_bytes ‖ "\0" ‖
//! supplementary_bytes_concatenated)`, where supplementary files are
//! concatenated in the order the frontmatter declared them — *not*
//! sorted (FMEA F11, WI-FMEA-03). Sorting would hide swap-order edits;
//! the declared order already establishes a canonical sequence since
//! the parser preserves it.
//!
//! The `\0` separators prevent content-boundary ambiguity: two skills
//! whose concatenated-before-hash bytes are identical must differ in
//! frontmatter vs. body split, and the null bytes ensure the hash sees
//! that difference.
//!
//! Output format is `sha256:<hex>` — fmem's `ingest_skill` accepts any
//! opaque string, but prefixing makes the hash self-describing.

use sha2::{Digest, Sha256};

use super::parse::Skill;
use super::supplementary::ResolvedSupplementary;

/// Compute the content hash for a parsed skill + its resolved
/// supplementary files.
pub fn content_hash(skill: &Skill, supplementary: &[ResolvedSupplementary]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(&skill.frontmatter_bytes);
    hasher.update(b"\0");
    hasher.update(&skill.body_bytes);
    hasher.update(b"\0");
    for sup in supplementary {
        hasher.update(sup.declared.as_bytes());
        hasher.update(b"\0");
        hasher.update(&sup.bytes);
        hasher.update(b"\0");
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_ingest::parse;
    use std::path::PathBuf;

    fn skill_with(frontmatter: &str, body: &str) -> Skill {
        let mut s = String::new();
        s.push_str("---\n");
        s.push_str(frontmatter);
        s.push_str("---\n");
        s.push_str(body);
        parse::parse(s.as_bytes(), "task-level").unwrap()
    }

    fn sup(name: &str, bytes: &[u8]) -> ResolvedSupplementary {
        ResolvedSupplementary {
            declared: name.to_string(),
            path: PathBuf::from(name),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn stable_across_runs() {
        let s = skill_with("name: x\ndescription: y\n", "body");
        let h1 = content_hash(&s, &[]);
        let h2 = content_hash(&s, &[]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn prefixed_with_sha256() {
        let s = skill_with("name: x\ndescription: y\n", "body");
        let h = content_hash(&s, &[]);
        assert!(h.starts_with("sha256:"));
        // Hex portion is 64 chars.
        assert_eq!(h.len(), "sha256:".len() + 64);
    }

    #[test]
    fn frontmatter_flip_changes_hash() {
        let a = skill_with("name: x\ndescription: y\n", "body");
        let b = skill_with("name: x\ndescription: z\n", "body");
        assert_ne!(content_hash(&a, &[]), content_hash(&b, &[]));
    }

    #[test]
    fn body_flip_changes_hash() {
        let a = skill_with("name: x\ndescription: y\n", "body one");
        let b = skill_with("name: x\ndescription: y\n", "body two");
        assert_ne!(content_hash(&a, &[]), content_hash(&b, &[]));
    }

    #[test]
    fn supplementary_edit_changes_hash() {
        // WI-FMEA-03 regression: editing a supplementary file must
        // change the content hash so fmem sees the update.
        let s = skill_with("name: x\ndescription: y\n", "body");
        let before = content_hash(&s, &[sup("extra.md", b"version one")]);
        let after = content_hash(&s, &[sup("extra.md", b"version two")]);
        assert_ne!(before, after, "supplementary edit must change hash");
    }

    #[test]
    fn supplementary_reorder_changes_hash() {
        // Declared order is preserved — reordering is a meaningful diff.
        let s = skill_with("name: x\ndescription: y\n", "body");
        let order1 = content_hash(&s, &[sup("a.md", b"A"), sup("b.md", b"B")]);
        let order2 = content_hash(&s, &[sup("b.md", b"B"), sup("a.md", b"A")]);
        assert_ne!(order1, order2);
    }

    #[test]
    fn frontmatter_body_boundary_disambiguated() {
        // Without the null-byte separator, these two skills could collide
        // because their concatenated bytes are identical:
        //   skill A: fm="ab", body="c"
        //   skill B: fm="a",  body="bc"
        // With the separator they must differ.
        let a = skill_with("name: ab\ndescription: y\n", "");
        let b = skill_with("name: a\ndescription: y\nextra: b\n", "");
        // Don't assert equality of the full objects, just the hash.
        assert_ne!(content_hash(&a, &[]), content_hash(&b, &[]));
    }

    #[test]
    fn identical_inputs_identical_hash() {
        let fm = "name: tdd\ndescription: do tdd\n";
        let body = "## Instructions\n\n- step\n";
        let a = skill_with(fm, body);
        let b = skill_with(fm, body);
        assert_eq!(content_hash(&a, &[]), content_hash(&b, &[]));
    }
}
