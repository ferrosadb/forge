//! `frg task delete` argument handling and fail-loud behaviour. The delete
//! semantics themselves (refusal without --force, links on both sides,
//! comments) are covered against a live cluster in
//! crates/tasks/tests/task_delete_live.rs.

use std::net::TcpListener;
use std::process::Command;

fn dead_contact_point() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr").to_string();
    drop(listener);
    addr
}

fn frg(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_frg"))
        .args(args)
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
fn delete_with_no_ids_fails_instead_of_doing_nothing() {
    let host = dead_contact_point();
    let (ok, _, stderr) = frg(&["task", "delete", "--cql-host", &host]);
    assert!(!ok, "no ids must not exit 0");
    assert!(stderr.contains("no task ids"), "stderr: {stderr}");
}

#[test]
fn delete_against_a_dead_board_exits_non_zero_and_names_the_host() {
    let host = dead_contact_point();
    let (ok, stdout, stderr) = frg(&["task", "delete", "t_00000001", "--cql-host", &host]);
    assert!(!ok, "stdout: {stdout}");
    assert!(stderr.contains(&host), "stderr must name {host}: {stderr}");
}

#[test]
fn ids_from_a_file_are_read_and_still_fail_loud_on_a_dead_board() {
    let host = dead_contact_point();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ids.txt");
    std::fs::write(&file, "# dupes from 2026-11-06\nt_00000001\n\nt_00000002\n").unwrap();
    let (ok, _, stderr) = frg(&[
        "task",
        "delete",
        "--ids-from",
        file.to_str().unwrap(),
        "--cql-host",
        &host,
    ]);
    assert!(!ok);
    assert!(
        !stderr.contains("no task ids"),
        "the file's ids were read: {stderr}"
    );
    assert!(stderr.contains(&host), "stderr: {stderr}");
}
