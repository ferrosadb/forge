//! End-to-end guard for the exact shape of the reported bug:
//!
//!     $ forge task list --limit 1        # nothing listening on the CQL port
//!     []                                 # exit 0, nothing on stderr
//!
//! A read against an unreachable board must exit non-zero, must not print an
//! empty JSON result on stdout, and must name the host and port it tried.
//! `/whats-next` and `/roadmap` read this board; an empty array they cannot
//! distinguish from a dead one turns "I could not look" into "there is no work".

use std::net::TcpListener;
use std::process::Command;

/// A `127.0.0.1:<port>` that nothing is listening on.
fn dead_contact_point() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr").to_string();
    drop(listener);
    addr
}

/// Run `frg task <args...> --cql-host <dead>` with the ambient config layers
/// neutralised, so the test asserts on the flag it passes and nothing else.
fn run_task_read(args: &[&str], host: &str) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_frg"))
        .arg("task")
        .args(args)
        .arg("--cql-host")
        .arg(host)
        .env_remove("FORGE_CQL_HOST")
        .env_remove("FORGE_DEBUG_STOP")
        .output()
        .expect("run frg");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn task_list_against_a_dead_board_exits_non_zero_and_prints_no_empty_array() {
    let host = dead_contact_point();
    let (ok, stdout, stderr) = run_task_read(&["list", "--limit", "1"], &host);

    assert!(
        !ok,
        "an unreachable board must not exit 0; stdout was {stdout:?}"
    );
    assert!(
        stdout.trim().is_empty(),
        "an unreachable board must print nothing on stdout, not a result; got {stdout:?}"
    );
    assert!(
        stderr.contains(&host),
        "stderr must name the host and port tried, got: {stderr}"
    );
}

#[test]
fn task_board_against_a_dead_board_exits_non_zero_and_names_the_host() {
    let host = dead_contact_point();
    let (ok, stdout, stderr) = run_task_read(&["board"], &host);

    assert!(
        !ok,
        "an unreachable board must not exit 0; stdout was {stdout:?}"
    );
    assert!(
        stdout.trim().is_empty(),
        "an unreachable board must print nothing on stdout; got {stdout:?}"
    );
    assert!(
        stderr.contains(&host),
        "stderr must name the host and port tried, got: {stderr}"
    );
}

#[test]
fn task_get_against_a_dead_board_exits_non_zero_and_names_the_host() {
    let host = dead_contact_point();
    let (ok, stdout, stderr) = run_task_read(&["get", "t_deadbeef"], &host);

    assert!(
        !ok,
        "an unreachable board must not exit 0; stdout was {stdout:?}"
    );
    assert!(
        stderr.contains(&host),
        "stderr must name the host and port tried, got: {stderr}"
    );
}
