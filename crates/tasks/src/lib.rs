//! Task tracking for forge — CQL-backed kanban with links and comments.
//!
//! Uses a thread-local tokio runtime to drive the async scylla driver
//! from synchronous forge code.  The `TaskStore` manages all CQL I/O.

mod config;
pub mod debug_stop;
mod schema;
mod store;
mod types;

pub use config::{resolve_cql_host, resolve_cql_hosts, DEFAULT_CQL_HOST};
pub use debug_stop::{apply_debug_stop, BoardHealth, DEBUG_STOP_CRITICAL};
pub use store::TaskStore;
pub use types::{
    Comment, CreateTaskRequest, KanbanBoard, Task, TaskFilter, TaskStatus, TaskWithLinks,
    UpdateTaskPatch,
};
