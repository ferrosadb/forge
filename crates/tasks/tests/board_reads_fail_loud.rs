//! A read against a board that cannot be reached must be an **error**, never an
//! empty result.
//!
//! The bug these tests lock out: `forge task list` printed `[]` and exited 0
//! when nothing was listening on the CQL port. A *query* answered "there is
//! nothing" when the truth was "I could not look", and every consumer of the
//! board (`/whats-next`, `/roadmap`, the defer-capture hook) was confidently
//! wrong in the same direction.
//!
//! No live cluster is needed: every case here is a socket the test owns, so
//! these run in CI alongside the unit tests.

use std::io::Read;
use std::net::TcpListener;
use std::time::{Duration, Instant};

use forge_tasks::TaskStore;

/// A `127.0.0.1:<port>` that nothing is listening on: bind to get a
/// kernel-assigned port, then drop the listener so a connect is refused.
fn dead_contact_point() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr").to_string();
    drop(listener);
    addr
}

/// A live TCP port that is not a CQL server: it accepts, reads nothing useful,
/// and hangs up. This is the shape of a stale port-forward (podman/gvproxy
/// keeps the port open after the container behind it dies), which is worse than
/// a refused connection precisely because the socket looks healthy.
fn non_cql_listener() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr").to_string();
    let handle = std::thread::spawn(move || {
        // Serve a bounded number of connections and then stop: the driver
        // retries, and an unbounded accept loop would never join.
        for _ in 0..64 {
            match listener.accept() {
                Ok((mut sock, _)) => {
                    let mut scratch = [0u8; 64];
                    let _ = sock.read(&mut scratch);
                    drop(sock);
                }
                Err(_) => return,
            }
        }
    });
    (addr, handle)
}

/// Connect and require failure, returning the fully rendered error chain.
/// `TaskStore` is not `Debug`, so `expect_err` is unavailable here.
fn connect_error(hosts: &[String], why: &str) -> String {
    match TaskStore::connect(hosts, None) {
        Ok(_) => panic!("{why}"),
        Err(e) => format!("{e:#}"),
    }
}

#[test]
fn connecting_to_a_dead_port_is_an_error_that_names_the_contact_point() {
    let addr = dead_contact_point();
    let rendered = connect_error(
        std::slice::from_ref(&addr),
        "an unreachable contact point must be an error, not a store whose reads look empty",
    );
    assert!(
        rendered.contains(&addr),
        "the error must name the host and port actually tried, got: {rendered}"
    );
}

#[test]
fn connecting_to_a_live_port_that_is_not_cql_is_an_error_that_names_the_contact_point() {
    let (addr, handle) = non_cql_listener();
    let started = Instant::now();
    let rendered = connect_error(
        std::slice::from_ref(&addr),
        "a port that is open but not speaking CQL must be an error",
    );
    assert!(
        rendered.contains(&addr),
        "the error must name the host and port actually tried, got: {rendered}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "a non-CQL endpoint must fail within a bounded time, not hang"
    );
    drop(handle);
}

#[test]
fn every_dead_contact_point_is_named_so_the_operator_knows_what_was_tried() {
    // With several bootstrap contact points the message has to say which set was
    // attempted; naming only the first would send the reader to the wrong host.
    let a = dead_contact_point();
    let b = dead_contact_point();
    let rendered = connect_error(
        &[a.clone(), b.clone()],
        "all contact points unreachable must be an error",
    );
    assert!(
        rendered.contains(&a) && rendered.contains(&b),
        "the error must name every contact point tried, got: {rendered}"
    );
}

#[test]
fn an_empty_contact_point_list_is_an_error_not_a_silent_default() {
    let rendered = connect_error(
        &[],
        "no contact points is a configuration error, not an empty board",
    );
    assert!(
        rendered.contains("contact point"),
        "the error should say what is missing, got: {rendered}"
    );
}
