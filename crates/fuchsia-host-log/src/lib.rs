/// Log levels exposed to components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Logging capability for task execution.
///
/// Routes component logs to the host's tracing infrastructure.
/// Implementations should include execution context (execution_id, node_id)
/// for correlation.
pub trait LogHost: Send + Sync {
    fn log(&self, level: LogLevel, message: &str);
}

/// Logger that routes to the `tracing` crate.
pub struct TracingLogHost {
    pub execution_id: String,
    pub node_id: String,
}

impl TracingLogHost {
    pub fn new(execution_id: String, node_id: String) -> Self {
        Self {
            execution_id,
            node_id,
        }
    }
}

impl LogHost for TracingLogHost {
    fn log(&self, level: LogLevel, message: &str) {
        match level {
            LogLevel::Trace => {
                tracing::trace!(
                    execution_id = %self.execution_id,
                    node_id = %self.node_id,
                    "{message}"
                );
            }
            LogLevel::Debug => {
                tracing::debug!(
                    execution_id = %self.execution_id,
                    node_id = %self.node_id,
                    "{message}"
                );
            }
            LogLevel::Info => {
                tracing::info!(
                    execution_id = %self.execution_id,
                    node_id = %self.node_id,
                    "{message}"
                );
            }
            LogLevel::Warn => {
                tracing::warn!(
                    execution_id = %self.execution_id,
                    node_id = %self.node_id,
                    "{message}"
                );
            }
            LogLevel::Error => {
                tracing::error!(
                    execution_id = %self.execution_id,
                    node_id = %self.node_id,
                    "{message}"
                );
            }
        }
    }
}
