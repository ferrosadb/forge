//! Task tracking for forge — CQL-backed kanban with links and comments.
//!
//! Uses a thread-local tokio runtime to drive the async scylla driver
//! from synchronous forge code.  The `TaskStore` manages all CQL I/O.

mod config;
pub mod debug_stop;
mod schema;
mod store;
mod types;

pub use config::{
    resolve_board_target, resolve_board_target_for_hosts, resolve_cql_host, resolve_cql_hosts,
    resolve_debug_stop, BoardCredential, BoardTarget, SettingSource, DEFAULT_CQL_HOST,
    DEFAULT_CQL_USERNAME,
};
pub use debug_stop::{apply_debug_stop, BoardHealth, DEBUG_STOP_CRITICAL};
pub use store::{default_page_limit, page_hint, TaskStore};
pub use types::{
    Comment, CreateTaskRequest, DeleteOutcome, KanbanBoard, Task, TaskFilter, TaskOrigin,
    TaskStatus, TaskWithLinks, UpdateTaskPatch,
};
