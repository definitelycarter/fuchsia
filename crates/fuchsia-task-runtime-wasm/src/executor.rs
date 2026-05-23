use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::RwLock;

use fuchsia_task_runtime::{Capabilities, ExecutorError, NodeExecutor, TaskInput, TaskOutput};
use wasmtime::component::Component;
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::p2::add_to_linker_async;

use crate::bindings::{Context, WasmTaskState, TaskComponent};

/// Configuration for the wasm executor.
#[derive(Debug, Clone)]
pub struct WasmExecutorConfig {
    /// Enable epoch-based interruption for timeouts.
    pub epoch_interruption: bool,
    /// Default epoch deadline (ticks before interrupt).
    pub default_epoch_deadline: u64,
}

impl Default for WasmExecutorConfig {
    fn default() -> Self {
        Self {
            epoch_interruption: true,
            default_epoch_deadline: u64::MAX,
        }
    }
}

/// Wasmtime implementation of `NodeExecutor`.
///
/// Owns the wasmtime `Engine` and an internal component cache.
/// Each `execute` call creates a fresh wasm instance from the
/// (cached) compiled component — VM memory is never shared between calls.
pub struct WasmExecutor {
    engine: Engine,
    cache: RwLock<HashMap<u64, Component>>,
    config: WasmExecutorConfig,
}

impl WasmExecutor {
    pub fn new(config: WasmExecutorConfig) -> Result<Self, ExecutorError> {
        let mut wasm_config = Config::new();
        wasm_config.async_support(true);
        wasm_config.epoch_interruption(config.epoch_interruption);
        wasm_config.wasm_component_model(true);

        let engine = Engine::new(&wasm_config).map_err(|e| ExecutorError::Compilation {
            message: format!("failed to create wasmtime engine: {e}"),
        })?;

        Ok(Self {
            engine,
            cache: RwLock::new(HashMap::new()),
            config,
        })
    }

    /// Get or compile a component from raw bytes.
    /// Uses a hash of the bytes as the cache key.
    fn get_or_compile(&self, bytes: &[u8]) -> Result<Component, ExecutorError> {
        let key = Self::hash_bytes(bytes);

        // Try read lock first.
        {
            let cache = self.cache.read().unwrap();
            if let Some(component) = cache.get(&key) {
                return Ok(component.clone());
            }
        }

        // Compile and cache.
        let component =
            Component::new(&self.engine, bytes).map_err(|e| ExecutorError::Compilation {
                message: e.to_string(),
            })?;

        {
            let mut cache = self.cache.write().unwrap();
            cache.insert(key, component.clone());
        }

        Ok(component)
    }

    fn hash_bytes(bytes: &[u8]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    }
}

impl NodeExecutor for WasmExecutor {
    fn execute<'a>(
        &'a self,
        bytes: &'a [u8],
        capabilities: Capabilities,
        input: TaskInput,
    ) -> Pin<Box<dyn Future<Output = Result<TaskOutput, ExecutorError>> + Send + 'a>> {
        Box::pin(async move {
            let component = self.get_or_compile(bytes)?;

            // Fresh store state per invocation — wasm memory is isolated.
            let state = WasmTaskState::from_capabilities(&capabilities);
            let mut store = Store::new(&self.engine, state);

            store.set_epoch_deadline(self.config.default_epoch_deadline);

            // Link host imports.
            let mut linker = wasmtime::component::Linker::<WasmTaskState>::new(&self.engine);
            add_to_linker_async(&mut linker).map_err(|e| ExecutorError::Compilation {
                message: format!("failed to link WASI: {e}"),
            })?;
            TaskComponent::add_to_linker::<WasmTaskState, WasmTaskState>(&mut linker, |s| s)
                .map_err(|e| ExecutorError::Compilation {
                    message: format!("failed to link fuchsia imports: {e}"),
                })?;

            // Instantiate.
            let instance =
                TaskComponent::instantiate_async(&mut store, &component, &linker)
                    .await
                    .map_err(|e| ExecutorError::Compilation {
                        message: format!("instantiation failed: {e}"),
                    })?;

            // Serialize input and call.
            let data = serde_json::to_string(&input.data).map_err(|e| {
                ExecutorError::Execution {
                    message: format!("failed to serialize input: {e}"),
                }
            })?;

            let ctx = Context {
                execution_id: String::new(),
                node_id: String::new(),
                task_id: String::new(),
            };

            let result = instance
                .fuchsia_task_task()
                .call_execute(&mut store, &ctx, &data)
                .await
                .map_err(|e| ExecutorError::Execution {
                    message: format!("wasm trap: {e}"),
                })?;

            match result {
                Ok(output) => {
                    let data: serde_json::Value =
                        serde_json::from_str(&output.data).map_err(|e| {
                            ExecutorError::Execution {
                                message: format!("invalid JSON output: {e}"),
                            }
                        })?;
                    Ok(TaskOutput { data })
                }
                Err(error_msg) => Err(ExecutorError::TaskError {
                    message: error_msg,
                }),
            }
        })
    }
}
