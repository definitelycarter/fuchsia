use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use thiserror::Error;
use tokio::sync::Mutex;

use fuchsia_host_config::ConfigHost;
use fuchsia_host_http::HttpHost;
use fuchsia_host_kv::KvStore;
use fuchsia_host_log::LogHost;

/// Structured input passed to a task execution.
#[derive(Debug, Clone)]
pub struct TaskInput {
    /// The resolved input data as JSON.
    pub data: serde_json::Value,
}

/// Structured output returned from a task execution.
#[derive(Debug, Clone)]
pub struct TaskOutput {
    /// The output data as JSON.
    pub data: serde_json::Value,
}

/// Errors from task execution.
#[derive(Debug, Error)]
pub enum ExecutorError {
    /// The runtime failed to load or compile the bytes.
    #[error("compilation failed: {message}")]
    Compilation { message: String },

    /// The task was instantiated but failed during execution (trap, exception, etc.).
    #[error("execution failed: {message}")]
    Execution { message: String },

    /// The task returned an application-level error (not a crash).
    #[error("task error: {message}")]
    TaskError { message: String },

    /// The task was cancelled (e.g. timeout).
    #[error("cancelled: {message}")]
    Cancelled { message: String },
}

/// Host capabilities shared across tasks within a workflow execution.
///
/// Capabilities are workflow-scoped: node 1 can write to KV, node 2 can
/// read from it. VM instances (wasm memory, lua state) are fresh per
/// invocation — capabilities are not.
///
/// The engine creates these once per workflow execution and passes
/// them (via Arc/Clone) to each task invocation.
#[derive(Clone)]
pub struct Capabilities {
    pub kv: Arc<Mutex<dyn KvStore>>,
    pub config: Arc<dyn ConfigHost>,
    pub log: Arc<dyn LogHost>,
    pub http: Arc<dyn HttpHost>,
}

/// The core runtime abstraction.
///
/// Takes raw bytes (wasm binary, lua source, js source) and executes them
/// as a task with the given capabilities and input.
///
/// Each implementation handles its own compilation, caching, and instance
/// lifecycle internally. VM instances are created fresh per invocation.
pub trait NodeExecutor: Send + Sync {
    /// Execute a task from raw bytes.
    ///
    /// - `bytes`: the component source (wasm binary, lua script, etc.)
    /// - `capabilities`: shared host services for this workflow execution
    /// - `input`: structured input data
    fn execute<'a>(
        &'a self,
        bytes: &'a [u8],
        capabilities: Capabilities,
        input: TaskInput,
    ) -> Pin<Box<dyn Future<Output = Result<TaskOutput, ExecutorError>> + Send + 'a>>;
}
