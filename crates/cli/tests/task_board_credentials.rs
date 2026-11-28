//! Which credential `frg task ...` logs in with, and what it says when the login
//! fails -- measured at the real entry point: the built `frg` binary, the real
//! CQL driver, and a CQL listener this test owns that records the credential the
//! driver sends and refuses it the way Ferrosa does.
//!
//! t_84fde1ee / t_4e5fdc3c: `TaskStore::connect` hardcoded the published
//! development login `ferrosa_admin` / `ferrosa_admin`. On a node installed with
//! a generated admin password every board command failed with
//! `Authentication failed - bad credentials`, there was no way to configure the
//! real one, and nothing said which settings had been used.
//!
//! Every child runs with an EMPTY environment (plus HOME and PATH) in a fresh
//! temp directory: the developer's shell may export `FORGE_CQL_HOST` for the live
//! board, which outranks `~/.config/forge.toml`, and a stray `.forge/config.toml`
//! above the cwd would do the same. The board host is always this test's own
//! listener, so no case can reach a real database.
//!
//! The listener speaks just enough of CQL v4 to get a password-authenticating
//! driver to send its AUTH_RESPONSE: OPTIONS -> SUPPORTED, STARTUP ->
//! AUTHENTICATE (PasswordAuthenticator), AUTH_RESPONSE -> ERROR 0x0100
//! "bad credentials". The authenticated half of a session is the live node's to
//! prove; what this file holds down is which credential reached the wire and how
//! the failure names its sources.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// (username, password) pairs the listener received, in order.
type Logins = Arc<Mutex<Vec<(String, String)>>>;

/// A CQL listener that records every login and refuses it.
struct RefusingCqlNode {
    addr: String,
    logins: Logins,
}

impl RefusingCqlNode {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local_addr").to_string();
        let logins: Logins = Arc::default();
        let seen = Arc::clone(&logins);
        std::thread::spawn(move || {
            // Bounded: the driver retries and opens a few connections, and an
            // unbounded accept loop would never end. The thread is left blocked
            // in accept when the test finishes, like the other socket fixtures.
            for _ in 0..64 {
                let Ok((sock, _)) = listener.accept() else {
                    return;
                };
                let seen = Arc::clone(&seen);
                std::thread::spawn(move || serve(sock, &seen));
            }
        });
        Self { addr, logins }
    }

    fn logins(&self) -> Vec<(String, String)> {
        self.logins.lock().expect("logins lock").clone()
    }
}

fn read_frame(sock: &mut TcpStream) -> Option<(u8, [u8; 2], Vec<u8>)> {
    let mut header = [0u8; 9];
    sock.read_exact(&mut header).ok()?;
    let len = u32::from_be_bytes([header[5], header[6], header[7], header[8]]) as usize;
    if len > 1 << 20 {
        return None;
    }
    let mut body = vec![0u8; len];
    sock.read_exact(&mut body).ok()?;
    Some((header[4], [header[2], header[3]], body))
}

fn write_frame(sock: &mut TcpStream, stream: [u8; 2], opcode: u8, body: &[u8]) -> bool {
    let mut frame = vec![0x84, 0x00, stream[0], stream[1], opcode];
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    sock.write_all(&frame).is_ok()
}

fn cql_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn supported_body() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&2u16.to_be_bytes());
    cql_string(&mut body, "CQL_VERSION");
    body.extend_from_slice(&1u16.to_be_bytes());
    cql_string(&mut body, "3.4.5");
    cql_string(&mut body, "COMPRESSION");
    body.extend_from_slice(&0u16.to_be_bytes());
    body
}

fn error_body(code: u32, message: &str) -> Vec<u8> {
    let mut body = code.to_be_bytes().to_vec();
    cql_string(&mut body, message);
    body
}

/// SASL PLAIN token: `\0username\0password`, inside a CQL `[bytes]`.
fn parse_plain_token(body: &[u8]) -> Option<(String, String)> {
    let len = i32::from_be_bytes(body.get(..4)?.try_into().ok()?);
    let token = body.get(4..4 + usize::try_from(len).ok()?)?;
    let mut parts = token.split(|b| *b == 0);
    let _authzid = parts.next()?;
    let user = String::from_utf8(parts.next()?.to_vec()).ok()?;
    let pass = String::from_utf8(parts.next()?.to_vec()).ok()?;
    Some((user, pass))
}

fn serve(mut sock: TcpStream, seen: &Logins) {
    const OPTIONS: u8 = 0x05;
    const STARTUP: u8 = 0x01;
    const AUTH_RESPONSE: u8 = 0x0F;
    while let Some((opcode, stream, body)) = read_frame(&mut sock) {
        let ok = match opcode {
            OPTIONS => write_frame(&mut sock, stream, 0x06, &supported_body()),
            STARTUP => {
                let mut auth = Vec::new();
                cql_string(&mut auth, "org.apache.cassandra.auth.PasswordAuthenticator");
                write_frame(&mut sock, stream, 0x03, &auth)
            }
            AUTH_RESPONSE => {
                if let Some(login) = parse_plain_token(&body) {
                    seen.lock().expect("logins lock").push(login);
                }
                write_frame(
                    &mut sock,
                    stream,
                    0x00,
                    &error_body(0x0100, "bad credentials"),
                );
                false
            }
            _ => write_frame(&mut sock, stream, 0x00, &error_body(0x000A, "unsupported")),
        };
        if !ok {
            return;
        }
    }
}

/// An isolated HOME and cwd: nothing from the developer's machine is visible.
struct Sandbox {
    home: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().expect("home tempdir"),
            work: tempfile::tempdir().expect("work tempdir"),
        }
    }

    fn global_config(&self) -> PathBuf {
        self.home.path().join(".config").join("forge.toml")
    }

    fn write_global_config(&self, body: &str) {
        std::fs::create_dir_all(self.home.path().join(".config")).expect("mkdir .config");
        std::fs::write(self.global_config(), body).expect("write forge.toml");
    }

    /// A password file with `mode`, holding `password` and a trailing newline
    /// (the shape `echo pw > file` and every installer produce).
    fn password_file(&self, name: &str, password: &str, mode: u32) -> PathBuf {
        let path = self.home.path().join(name);
        std::fs::write(&path, format!("{password}\n")).expect("write password file");
        set_mode(&path, mode);
        path
    }

    /// `frg task list --limit 1 [extra...]` with only HOME and PATH set.
    fn frg_task_list(&self, extra: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_frg"));
        cmd.args(["task", "list", "--limit", "1"])
            .args(extra)
            .current_dir(self.work.path())
            .env_clear()
            .env("HOME", self.home.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("run frg");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

/// Every role the listener saw, so a wrong-credential failure prints them.
fn roles(logins: &[(String, String)]) -> Vec<&str> {
    logins.iter().map(|(user, _)| user.as_str()).collect()
}

#[test]
fn the_configured_password_file_is_the_credential_sent_and_the_failure_names_its_config() {
    let node = RefusingCqlNode::start();
    let sandbox = Sandbox::new();
    let secret = sandbox.password_file("board.secret", "fixture-board-pass", 0o600);
    sandbox.write_global_config(&format!(
        "cql_host = \"{}\"\ncql_username = \"board_writer\"\ncql_password_file = \"{}\"\n",
        node.addr,
        secret.display()
    ));

    let (ok, stdout, stderr) = sandbox.frg_task_list(&[], &[]);

    assert!(!ok, "a refused login must not exit 0; stdout {stdout:?}");
    let logins = node.logins();
    assert!(
        !logins.is_empty(),
        "frg never sent a login; stderr: {stderr}"
    );
    assert!(
        logins
            .iter()
            .all(|(user, pass)| user == "board_writer" && pass == "fixture-board-pass"),
        "every login must use cql_username and the password file's contents (newline trimmed); \
         roles sent: {:?}",
        roles(&logins)
    );
    let config = sandbox.global_config().display().to_string();
    assert!(
        stderr.contains(&config),
        "the failure must name the config file that supplied host and credential ({config}): {stderr}"
    );
    assert!(
        stderr.contains("board_writer"),
        "the failure must name the role: {stderr}"
    );
    assert!(
        !stderr.contains("fixture-board-pass"),
        "the failure must never print the password: {stderr}"
    );
}

#[test]
fn environment_credentials_outrank_the_config_file_and_are_named() {
    let node = RefusingCqlNode::start();
    let sandbox = Sandbox::new();
    let file_secret = sandbox.password_file("file.secret", "fixture-file-pass", 0o600);
    let env_secret = sandbox.password_file("env.secret", "fixture-env-pass", 0o600);
    sandbox.write_global_config(&format!(
        "cql_host = \"{}\"\ncql_username = \"file_role\"\ncql_password_file = \"{}\"\n",
        node.addr,
        file_secret.display()
    ));
    let env_secret = env_secret.display().to_string();

    let (ok, _stdout, stderr) = sandbox.frg_task_list(
        &[],
        &[
            ("FORGE_CQL_USERNAME", "env_role"),
            ("FORGE_CQL_PASSWORD_FILE", env_secret.as_str()),
        ],
    );

    assert!(!ok);
    let logins = node.logins();
    assert!(
        !logins.is_empty(),
        "frg never sent a login; stderr: {stderr}"
    );
    assert!(
        logins
            .iter()
            .all(|(user, pass)| user == "env_role" && pass == "fixture-env-pass"),
        "the environment pair must win over the config file; roles sent: {:?}",
        roles(&logins)
    );
    assert!(
        stderr.contains("FORGE_CQL_PASSWORD_FILE"),
        "the failure must say the credential came from the environment: {stderr}"
    );
    assert!(stderr.contains("env_role"), "{stderr}");
    assert!(!stderr.contains("fixture-env-pass"), "{stderr}");
}

#[test]
fn with_no_credential_configured_the_development_default_is_used_and_said() {
    let node = RefusingCqlNode::start();
    let sandbox = Sandbox::new();

    let (ok, _stdout, stderr) = sandbox.frg_task_list(&["--cql-host", &node.addr], &[]);

    assert!(!ok);
    let logins = node.logins();
    assert!(
        !logins.is_empty(),
        "frg never sent a login; stderr: {stderr}"
    );
    assert!(
        logins
            .iter()
            .all(|(user, pass)| user == "ferrosa_admin" && pass == "ferrosa_admin"),
        "existing installs keep the published development login; roles sent: {:?}",
        roles(&logins)
    );
    assert!(
        stderr.contains("built-in development credential ferrosa_admin"),
        "the failure must say the credential was the built-in fallback: {stderr}"
    );
    assert!(
        stderr.contains("--cql-host"),
        "the failure must say the host came from the flag: {stderr}"
    );
}

#[test]
fn a_missing_password_file_is_a_loud_error_naming_it_and_no_login_is_tried() {
    let node = RefusingCqlNode::start();
    let sandbox = Sandbox::new();
    let gone = sandbox.home.path().join("no-such.secret");
    sandbox.write_global_config(&format!(
        "cql_host = \"{}\"\ncql_username = \"board_writer\"\ncql_password_file = \"{}\"\n",
        node.addr,
        gone.display()
    ));

    let (ok, stdout, stderr) = sandbox.frg_task_list(&[], &[]);

    assert!(!ok, "a missing password file must fail; stdout {stdout:?}");
    assert!(
        stderr.contains(&gone.display().to_string()),
        "the error must name the password file: {stderr}"
    );
    assert!(
        node.logins().is_empty(),
        "no login may be attempted with a credential that could not be read; roles sent: {:?}",
        roles(&node.logins())
    );
}

#[test]
fn an_empty_password_file_is_a_loud_error_naming_it() {
    let node = RefusingCqlNode::start();
    let sandbox = Sandbox::new();
    let empty = sandbox.password_file("empty.secret", "", 0o600);
    sandbox.write_global_config(&format!(
        "cql_host = \"{}\"\ncql_username = \"board_writer\"\ncql_password_file = \"{}\"\n",
        node.addr,
        empty.display()
    ));

    let (ok, _stdout, stderr) = sandbox.frg_task_list(&[], &[]);

    assert!(!ok);
    assert!(
        stderr.contains(&empty.display().to_string()) && stderr.contains("empty"),
        "the error must say the password file is empty and name it: {stderr}"
    );
    assert!(node.logins().is_empty(), "no login with an empty password");
}

#[test]
fn an_inline_password_in_config_is_refused_without_echoing_it() {
    let node = RefusingCqlNode::start();
    let sandbox = Sandbox::new();
    sandbox.write_global_config(&format!(
        "cql_host = \"{}\"\ncql_username = \"board_writer\"\ncql_password = \"fixture-inline-pass\"\n",
        node.addr
    ));

    let (ok, _stdout, stderr) = sandbox.frg_task_list(&[], &[]);

    assert!(!ok, "a config holding a password must be refused");
    assert!(
        stderr.contains("cql_password_file"),
        "the error must point at the supported key: {stderr}"
    );
    assert!(
        !stderr.contains("fixture-inline-pass"),
        "the refusal must not echo the password: {stderr}"
    );
    assert!(node.logins().is_empty(), "no login from a refused config");
}

#[cfg(unix)]
#[test]
fn a_password_file_readable_by_others_warns_but_is_still_used() {
    let node = RefusingCqlNode::start();
    let sandbox = Sandbox::new();
    let loose = sandbox.password_file("loose.secret", "fixture-loose-pass", 0o644);
    sandbox.write_global_config(&format!(
        "cql_host = \"{}\"\ncql_username = \"board_writer\"\ncql_password_file = \"{}\"\n",
        node.addr,
        loose.display()
    ));

    let (_ok, _stdout, stderr) = sandbox.frg_task_list(&[], &[]);

    assert!(
        stderr.contains("warning") && stderr.contains(&loose.display().to_string()),
        "a group/other-readable password file must be warned about by name: {stderr}"
    );
    assert!(
        node.logins()
            .iter()
            .any(|(user, pass)| user == "board_writer" && pass == "fixture-loose-pass"),
        "a loose mode warns, it does not refuse; roles sent: {:?}",
        roles(&node.logins())
    );
}
