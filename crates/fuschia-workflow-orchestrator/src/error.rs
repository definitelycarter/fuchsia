//! Orchestrator error types.

use fuschia_task_runtime::ExecutorError;

/// Errors that can occur during workflow orchestration.
#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    /// Execution was cancelled.
    #[error("execution cancelled")]
    Cancelled,

    /// Failed to serialize component input.
    #[error("failed to serialize input: {message}")]
    InputSerialization { message: String },

    /// Task execution failed via the RuntimeRegistry.
    #[error("task execution failed: {source}")]
    TaskExecution {
        #[source]
        source: ExecutorError,
    },

    /// Failed to parse component output.
    #[error("invalid component output: {message}")]
    InvalidOutput { message: String },

    /// Failed to resolve node inputs (template rendering or type coercion).
    #[error("input resolution failed for node '{node_id}': {message}")]
    InputResolution { node_id: String, message: String },

    /// Failed to load component bytes.
    #[error("failed to load component for node '{node_id}': {message}")]
    ComponentLoad { node_id: String, message: String },

    /// Invalid workflow graph structure.
    #[error("invalid graph: {message}")]
    InvalidGraph { message: String },
}
