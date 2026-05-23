//! Lua implementation of `NodeExecutor`.
//!
//! Each `execute` call creates a fresh Lua VM, loads the script,
//! wires up capabilities, and calls the script's `execute(ctx, data)` function.
//! No caching — Lua source loading is cheap.

use std::future::Future;
use std::pin::Pin;

use fuchsia_task_runtime::{Capabilities, ExecutorError, NodeExecutor, TaskInput, TaskOutput};

use crate::bindings::register_all;

/// Lua implementation of `NodeExecutor`.
///
/// Stateless — each invocation gets a fresh Lua VM.
pub struct LuaExecutor;

impl LuaExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LuaExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeExecutor for LuaExecutor {
    fn execute<'a>(
        &'a self,
        bytes: &'a [u8],
        capabilities: Capabilities,
        input: TaskInput,
    ) -> Pin<Box<dyn Future<Output = Result<TaskOutput, ExecutorError>> + Send + 'a>> {
        Box::pin(async move {
            // Parse bytes as UTF-8 Lua source.
            let source = std::str::from_utf8(bytes).map_err(|e| ExecutorError::Compilation {
                message: format!("invalid UTF-8: {e}"),
            })?;

            // Fresh Lua state per invocation.
            let lua = mlua::Lua::new();

            // Register capability bindings (kv, config, log).
            register_all(&lua, &capabilities).map_err(|e| ExecutorError::Compilation {
                message: format!("failed to register bindings: {e}"),
            })?;

            // Load and execute the script to define the `execute` function.
            lua.load(source).exec().map_err(|e| ExecutorError::Compilation {
                message: format!("lua load failed: {e}"),
            })?;

            // Get the execute function.
            let execute_fn: mlua::Function =
                lua.globals().get("execute").map_err(|_| ExecutorError::Compilation {
                    message: "script must define an `execute(ctx, data)` function".into(),
                })?;

            // Build context table.
            let ctx = lua.create_table().map_err(|e| ExecutorError::Execution {
                message: format!("failed to create context table: {e}"),
            })?;
            ctx.set("execution_id", "").map_err(|e| ExecutorError::Execution {
                message: format!("failed to set context field: {e}"),
            })?;
            ctx.set("node_id", "").map_err(|e| ExecutorError::Execution {
                message: format!("failed to set context field: {e}"),
            })?;
            ctx.set("task_id", "").map_err(|e| ExecutorError::Execution {
                message: format!("failed to set context field: {e}"),
            })?;

            // Serialize input to JSON string.
            let data =
                serde_json::to_string(&input.data).map_err(|e| ExecutorError::Execution {
                    message: format!("failed to serialize input: {e}"),
                })?;

            // Call execute(ctx, data) -> result string.
            let result: String = execute_fn.call((ctx, data)).map_err(|e| {
                ExecutorError::TaskError {
                    message: e.to_string(),
                }
            })?;

            // Parse output JSON.
            let data: serde_json::Value =
                serde_json::from_str(&result).map_err(|e| ExecutorError::Execution {
                    message: format!("invalid JSON output: {e}"),
                })?;

            Ok(TaskOutput { data })
        })
    }
}
