use std::collections::HashMap;
use std::sync::Arc;

use crate::{Capabilities, ExecutorError, NodeExecutor, TaskInput, TaskOutput};

/// Supported runtime backends.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum RuntimeType {
    #[cfg(feature = "wasm")]
    Wasm,
    #[cfg(feature = "lua")]
    Lua,
}

/// Routes task execution to the appropriate runtime backend.
///
/// Executors are registered by `RuntimeType`. The registry holds
/// `Arc<dyn NodeExecutor>` so the same executor instance can serve
/// concurrent tasks.
pub struct RuntimeRegistry {
    executors: HashMap<RuntimeType, Arc<dyn NodeExecutor>>,
}

impl RuntimeRegistry {
    pub fn new() -> Self {
        Self {
            executors: HashMap::new(),
        }
    }

    /// Register an executor for a runtime type.
    pub fn register(&mut self, runtime_type: RuntimeType, executor: Arc<dyn NodeExecutor>) {
        self.executors.insert(runtime_type, executor);
    }

    /// Execute a task using the specified runtime.
    pub async fn execute(
        &self,
        runtime_type: RuntimeType,
        bytes: &[u8],
        capabilities: Capabilities,
        input: TaskInput,
    ) -> Result<TaskOutput, ExecutorError> {
        let executor =
            self.executors
                .get(&runtime_type)
                .ok_or_else(|| ExecutorError::Execution {
                    message: format!("no executor registered for {runtime_type:?}"),
                })?;

        executor.execute(bytes, capabilities, input).await
    }

    /// Check whether a runtime type has a registered executor.
    pub fn has(&self, runtime_type: RuntimeType) -> bool {
        self.executors.contains_key(&runtime_type)
    }
}

impl Default for RuntimeRegistry {
    fn default() -> Self {
        Self::new()
    }
}
