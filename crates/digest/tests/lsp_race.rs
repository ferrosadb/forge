//! The LSP race may only ever be a speed-up, never a cost.
//!
//! `find_definition` once blocked on a language server for up to 5s + 20s
//! before falling back, so a cold server made the tool 25s slower than the
//! walk. These tests pin the two properties that stop that recurring.
//!
//! **These are regression guards, not the red that produced the fix.** Measured
//! here: they pass against the sequential design too, because in this
//! environment rust-analyzer yields no matches quickly, so `recv_timeout`
//! returns rather than waiting. What they *do* hold down is the cap: raise
//! `SERVER_START_TIMEOUT`/`REQUEST_TIMEOUT` past ~2.5s and they fail. The
//! structural claim — the walk's answer is returned without waiting for the
//! LSP — is the `try_recv` in `lookup_symbol`, and proving it needs a
//! deliberately slow LSP injected, which is not wired.

use forge_digest::lookup::lookup_symbol;
use std::time::Instant;

/// The walk is the floor: with no language server reachable, the lookup must
/// still answer at walk speed.
#[test]
fn a_lookup_without_a_language_server_answers_at_walk_speed() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("a.rs"), "pub fn alpha() {}\n").expect("write");
    let started = Instant::now();
    let result = lookup_symbol("alpha", dir.path()).expect("lookup");
    let elapsed = started.elapsed();

    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.method, "grep", "no LSP was available here");
    assert!(
        elapsed.as_millis() < 2500,
        "a lookup with no language server paid {}ms — the LSP cap leaked into \
         the fallback path",
        elapsed.as_millis()
    );
}

/// A tree large enough that the walk alone is measurable: the race must not
/// add the LSP's cap on top of it.
#[test]
fn the_race_never_adds_the_language_servers_cap_to_the_walk() {
    let dir = tempfile::tempdir().expect("temp dir");
    for i in 0..300 {
        std::fs::write(
            dir.path().join(format!("f{i}.rs")),
            format!("pub fn f{i}() {{}}\n"),
        )
        .expect("write");
    }
    let started = Instant::now();
    let result = lookup_symbol("f42", dir.path()).expect("lookup");
    let elapsed = started.elapsed();
    assert_eq!(result.matches.len(), 1);
    assert!(
        elapsed.as_millis() < 2500,
        "the walk took {}ms with a 1.2s+1.8s LSP cap beside it",
        elapsed.as_millis()
    );
}
