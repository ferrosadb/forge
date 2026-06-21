//! Secret-scan gate for skill ingest.
//!
//! Runs `forge-secret-scan` over each SKILL.md body before passing it to
//! fmem. Any finding aborts ingest of that skill with a clear error
//! naming the file and offset (threat-model I1 — secrets accidentally
//! embedded in skill bodies).
//!
//! The scan runs in-memory — we don't write the skill to a temp file —
//! so the error path never echoes raw body content (threat-model I2).

use forge_secret_scan::Finding;

use super::parse::Skill;

/// Result of scanning a parsed skill.
#[derive(Debug)]
pub struct SecretFinding {
    /// Origin label — typically the SKILL.md path.
    pub origin: String,
    /// Raw findings from the secret-scan crate. Already masked.
    pub findings: Vec<Finding>,
}

impl std::fmt::Display for SecretFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} secret(s) in {}: ", self.findings.len(), self.origin)?;
        for (i, fnd) in self.findings.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            write!(f, "line {} ({})", fnd.line, fnd.label)?;
        }
        Ok(())
    }
}

impl std::error::Error for SecretFinding {}

/// Scan a parsed skill's body + frontmatter for secrets. Returns
/// `Ok(())` if clean, `Err(SecretFinding)` otherwise.
///
/// We concatenate frontmatter + body before scanning because either
/// region can legitimately contain text — e.g. someone pastes an
/// example snippet into the body, or accidentally commits a `keywords:`
/// value that matches a token pattern.
pub fn check_skill(origin: &str, skill: &Skill) -> Result<(), SecretFinding> {
    let mut combined =
        String::with_capacity(skill.frontmatter_bytes.len() + skill.body_bytes.len());
    // We already validated UTF-8 at parse time, so from_utf8 is safe.
    // Fall back to String::from_utf8_lossy in case of a bug — the
    // secret scan is defensive anyway.
    combined.push_str(
        std::str::from_utf8(&skill.frontmatter_bytes).unwrap_or("<non-utf8 frontmatter>"),
    );
    combined.push('\n');
    combined.push_str(std::str::from_utf8(&skill.body_bytes).unwrap_or("<non-utf8 body>"));

    let findings = forge_secret_scan::scan_text(origin, &combined);
    if findings.is_empty() {
        Ok(())
    } else {
        Err(SecretFinding {
            origin: origin.to_string(),
            findings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_ingest::parse;

    fn skill_with(body: &str) -> Skill {
        let raw = format!("---\nname: x\ndescription: y\n---\n{body}");
        parse::parse(raw.as_bytes(), "task-level").unwrap()
    }

    #[test]
    fn clean_skill_passes() {
        let s = skill_with("## Instructions\n\n- Do a thing\n- Do another\n");
        check_skill("/x/SKILL.md", &s).unwrap();
    }

    #[test]
    fn aws_key_in_body_triggers() {
        // Synthetic AWS access key — matches secret-scan's AKIA rule.
        let s = skill_with(concat!(
            "\nExample usage: ",
            "AK",
            "IAIOSFODNN7EXAMPLE (do not commit real keys)\n"
        ));
        let err = check_skill("/x/SKILL.md", &s).unwrap_err();
        assert_eq!(err.origin, "/x/SKILL.md");
        assert!(!err.findings.is_empty());
        // Error message must name the file but must NOT echo the body
        // content (threat-model I2).
        let msg = err.to_string();
        assert!(msg.contains("/x/SKILL.md"));
        assert!(!msg.contains(concat!("AK", "IAIOSFODNN7EXAMPLE")));
    }

    #[test]
    fn private_key_header_triggers() {
        let s = skill_with(concat!("\n-----BEGIN RSA ", "PRIVATE KEY-----\n"));
        let err = check_skill("/x/SKILL.md", &s).unwrap_err();
        assert!(!err.findings.is_empty());
    }
}
