use std::collections::HashMap;
use std::sync::Arc;

use fuchsia_host_config::MapConfig;
use fuchsia_host_http::{HttpPolicy, ReqwestHttpHost};
use fuchsia_host_kv::{InMemoryKvStore, KvStore};
use fuchsia_host_log::TracingLogHost;
use fuchsia_task_runtime::{Capabilities, ExecutorError, NodeExecutor, TaskInput};
use fuchsia_task_runtime_lua::LuaExecutor;
use tokio::sync::Mutex;

fn make_capabilities() -> Capabilities {
    Capabilities {
        kv: Arc::new(Mutex::new(InMemoryKvStore::new())),
        config: Arc::new(MapConfig::new(HashMap::new())),
        log: Arc::new(TracingLogHost::new(
            "test-exec".to_string(),
            "test-node".to_string(),
        )),
        http: Arc::new(ReqwestHttpHost::new(HttpPolicy::default())),
    }
}

/// Lua equivalent of the test-task-component: uses KV, config, and log,
/// then returns a JSON object with input echo, stored_matches, and config_value.
const TEST_TASK_LUA: &str = r#"
function execute(ctx, data)
    log.log("info", "executing task")

    kv.set("last_input", data)
    local stored = kv.get("last_input")

    local config_val = config.get("test_key")

    local stored_matches = "false"
    if stored == data then
        stored_matches = "true"
    end

    local config_json = "null"
    if config_val then
        config_json = '"' .. config_val .. '"'
    end

    return '{"input": ' .. data .. ', "stored_matches": ' .. stored_matches .. ', "config_value": ' .. config_json .. '}'
end
"#;

#[tokio::test]
async fn test_execute_task_via_node_executor() {
    let executor = LuaExecutor::new();
    let capabilities = make_capabilities();

    let input = TaskInput {
        data: serde_json::json!({"message": "hello from lua"}),
    };

    let output = executor
        .execute(TEST_TASK_LUA.as_bytes(), capabilities, input)
        .await
        .expect("execution failed");

    assert_eq!(output.data["input"]["message"], "hello from lua");
    assert_eq!(output.data["stored_matches"], true);
}

#[tokio::test]
async fn test_shared_kv_across_executions() {
    let executor = LuaExecutor::new();

    // Shared KV across both executions.
    let kv: Arc<Mutex<dyn KvStore>> = Arc::new(Mutex::new(InMemoryKvStore::new()));

    let caps1 = Capabilities {
        kv: Arc::clone(&kv),
        config: Arc::new(MapConfig::new(HashMap::new())),
        log: Arc::new(TracingLogHost::new(
            "exec-1".to_string(),
            "node-1".to_string(),
        )),
        http: Arc::new(ReqwestHttpHost::new(HttpPolicy::default())),
    };

    let caps2 = Capabilities {
        kv: Arc::clone(&kv),
        config: Arc::new(MapConfig::new(HashMap::new())),
        log: Arc::new(TracingLogHost::new(
            "exec-1".to_string(),
            "node-2".to_string(),
        )),
        http: Arc::new(ReqwestHttpHost::new(HttpPolicy::default())),
    };

    // First task writes to KV.
    let output1 = executor
        .execute(
            TEST_TASK_LUA.as_bytes(),
            caps1,
            TaskInput {
                data: serde_json::json!({"value": "first"}),
            },
        )
        .await
        .expect("first execution failed");

    assert_eq!(output1.data["stored_matches"], true);

    // Second task overwrites the same key.
    let output2 = executor
        .execute(
            TEST_TASK_LUA.as_bytes(),
            caps2,
            TaskInput {
                data: serde_json::json!({"value": "second"}),
            },
        )
        .await
        .expect("second execution failed");

    assert_eq!(output2.data["stored_matches"], true);

    // Verify KV has the second value (shared state).
    let store = kv.lock().await;
    let stored = store.get("last_input").await;
    assert_eq!(
        stored,
        Some(r#"{"value":"second"}"#.to_string()),
        "KV should have the second task's input since they share state"
    );
}

#[tokio::test]
async fn test_config_passed_to_component() {
    let executor = LuaExecutor::new();

    let mut config_values = HashMap::new();
    config_values.insert("test_key".to_string(), "test_value".to_string());

    let capabilities = Capabilities {
        kv: Arc::new(Mutex::new(InMemoryKvStore::new())),
        config: Arc::new(MapConfig::new(config_values)),
        log: Arc::new(TracingLogHost::new("exec".to_string(), "node".to_string())),
        http: Arc::new(ReqwestHttpHost::new(HttpPolicy::default())),
    };

    let output = executor
        .execute(
            TEST_TASK_LUA.as_bytes(),
            capabilities,
            TaskInput {
                data: serde_json::json!({"hello": "world"}),
            },
        )
        .await
        .expect("execution failed");

    assert_eq!(output.data["config_value"], "test_value");
}

#[tokio::test]
async fn test_script_error_handling() {
    let executor = LuaExecutor::new();
    let capabilities = make_capabilities();

    let script = br#"
function execute(ctx, data)
    error("something broke")
end
"#;

    let result = executor
        .execute(
            script,
            capabilities,
            TaskInput {
                data: serde_json::json!({}),
            },
        )
        .await;

    let err = result.unwrap_err();
    assert!(
        matches!(err, ExecutorError::TaskError { .. }),
        "expected TaskError, got: {err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("something broke"), "error should contain message: {msg}");
}

#[tokio::test]
async fn test_invalid_lua_source() {
    let executor = LuaExecutor::new();
    let capabilities = make_capabilities();

    let result = executor
        .execute(
            b"this is not valid lua }{}{",
            capabilities,
            TaskInput {
                data: serde_json::json!({}),
            },
        )
        .await;

    let err = result.unwrap_err();
    assert!(
        matches!(err, ExecutorError::Compilation { .. }),
        "expected Compilation error, got: {err:?}"
    );
}

#[tokio::test]
async fn test_missing_execute_function() {
    let executor = LuaExecutor::new();
    let capabilities = make_capabilities();

    let script = b"-- valid lua but no execute function\nlocal x = 42\n";

    let result = executor
        .execute(
            script,
            capabilities,
            TaskInput {
                data: serde_json::json!({}),
            },
        )
        .await;

    let err = result.unwrap_err();
    assert!(
        matches!(err, ExecutorError::Compilation { .. }),
        "expected Compilation error, got: {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("execute"),
        "error should mention missing execute function: {msg}"
    );
}
