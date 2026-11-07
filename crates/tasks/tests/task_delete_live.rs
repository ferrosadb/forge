//! `TaskStore::delete_task` against the live 3-node dev cluster, in a fresh
//! random tenant so the real board is never touched:
//!   cargo test -p forge-tasks --test task_delete_live -- --ignored --nocapture
//!
//! Why the command exists: on 2026-11-06 one board held 45,566 duplicate
//! hook-created tasks and forge had no way to remove one, so cleaning it up
//! meant raw CQL that knew nothing about links or comments.

use forge_tasks::{CreateTaskRequest, TaskOrigin, TaskStore};

fn store() -> TaskStore {
    let hosts = vec![
        "127.0.0.1:19042".to_string(),
        "127.0.0.1:19043".to_string(),
        "127.0.0.1:19044".to_string(),
    ];
    let tenant = uuid::Uuid::new_v4().to_string();
    TaskStore::connect(&hosts, Some(&tenant)).expect("connect to the dev cluster on 19042-19044")
}

fn create(store: &TaskStore, title: &str) -> String {
    store
        .create_task(CreateTaskRequest {
            title: title.to_string(),
            origin: TaskOrigin::Agent,
            body: None,
            assignee: None,
            reviewer: None,
            priority: None,
            workspace_kind: None,
            workspace_path: None,
            metadata: None,
            created_by: Some("task_delete_live".to_string()),
            skills: None,
            parents: None,
        })
        .expect("create task")
        .task_id
}

#[test]
#[ignore = "requires the live 3-node dev cluster on 19042-19044"]
fn deleting_a_plain_task_removes_it() {
    let store = store();
    let id = create(&store, "plain");
    let outcome = store.delete_task(&id, false).expect("delete");
    assert_eq!(outcome.task_id, id);
    assert_eq!((outcome.links_removed, outcome.comments_removed), (0, 0));
    assert!(store.find_task(&id).unwrap().is_none(), "the task is gone");
}

#[test]
#[ignore = "requires the live 3-node dev cluster on 19042-19044"]
fn deleting_a_missing_task_is_an_error_not_a_no_op() {
    let store = store();
    let err = store.delete_task("t_doesnotexist", false).unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}

#[test]
#[ignore = "requires the live 3-node dev cluster on 19042-19044"]
fn a_linked_or_commented_task_is_refused_without_force() {
    let store = store();
    let parent = create(&store, "parent");
    let child = create(&store, "child");
    store.link_tasks(&parent, &child, "child").unwrap();
    store.add_comment(&child, "test", "a note").unwrap();

    let err = store.delete_task(&child, false).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("1 link") && text.contains("1 comment") && text.contains("--force"),
        "{text}"
    );
    assert!(
        store.find_task(&child).unwrap().is_some(),
        "a refused delete leaves the task"
    );
}

#[test]
#[ignore = "requires the live 3-node dev cluster on 19042-19044"]
fn force_removes_the_task_its_links_on_both_sides_and_its_comments() {
    let store = store();
    let parent = create(&store, "parent");
    let child = create(&store, "child");
    store.link_tasks(&parent, &child, "child").unwrap();
    store.add_comment(&child, "test", "a note").unwrap();

    let outcome = store.delete_task(&child, true).expect("forced delete");
    assert_eq!((outcome.links_removed, outcome.comments_removed), (1, 1));
    assert!(store.find_task(&child).unwrap().is_none());
    let parent_now = store
        .find_task(&parent)
        .unwrap()
        .expect("the parent survives");
    assert!(
        parent_now.children.is_empty(),
        "no dangling link on the parent: {:?}",
        parent_now.children
    );
}
