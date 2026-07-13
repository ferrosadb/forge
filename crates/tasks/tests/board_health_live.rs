//! Live check that `board_health` reflects the real cluster via the driver's
//! `system.peers`-derived topology. Run with the 3-node dev cluster up:
//!   cargo test -p forge-tasks --test board_health_live -- --ignored --nocapture

use forge_tasks::TaskStore;

#[test]
#[ignore = "requires the live 3-node dev cluster on 19042-19044"]
fn board_health_sees_all_three_nodes() {
    let hosts = vec![
        "127.0.0.1:19042".to_string(),
        "127.0.0.1:19043".to_string(),
        "127.0.0.1:19044".to_string(),
    ];
    let store = TaskStore::connect(&hosts, None).expect("connect");

    // The driver may still be populating topology right after connect; give it a
    // brief, bounded window to discover all peers from system.peers.
    let mut health = store.board_health();
    for _ in 0..20 {
        if health.nodes_total >= 3 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        health = store.board_health();
    }

    eprintln!("board_health = {health:?}");
    assert!(
        health.nodes_total >= 3,
        "driver should discover all 3 nodes via system.peers, got {health:?}"
    );
    assert_eq!(health.nodes_up, health.nodes_total, "all nodes healthy");
    assert!(health.quorum());
}
