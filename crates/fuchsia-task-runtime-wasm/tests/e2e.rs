use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fuchsia_host_config::MapConfig;
use fuchsia_host_http::{HttpPolicy, ReqwestHttpHost};
use fuchsia_host_kv::{InMemoryKvStore, KvStore};
use fuchsia_host_log::TracingLogHost;
use fuchsia_task_runtime::{Capabilities, NodeExecutor, TaskInput};
use fuchsia_task_runtime_wasm::{WasmExecutor, WasmExecutorConfig};
use tokio::sync::Mutex;

const TEST_COMPONENT_PATH: &str =
    "../../test-components/test-task-component/target/wasm32-wasip1/release/test_task_component.wasm";

fn load_test_component() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(TEST_COMPONENT_PATH);
    assert!(
        path.exists(),
        "Test component not found at {path:?}. Run `cargo component build --release` in test-components/test-task-component first."
    );
    std::fs::read(&path).expect("failed to read wasm component")
}

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

#[tokio::test]
async fn test_execute_task_via_node_executor() {
    let executor = WasmExecutor::new(WasmExecutorConfig::default())
        .expect("failed to create wasm executor");

    let bytes = load_test_component();
    let capabilities = make_capabilities();

    let input = TaskInput {
        data: serde_json::json!({"message": "hello from e2e"}),
    };

    let output = executor
        .execute(&bytes, capabilities, input)
        .await
        .expect("execution failed");

    assert_eq!(output.data["input"]["message"], "hello from e2e");
    assert_eq!(output.data["stored_matches"], true);
}

#[tokio::test]
async fn test_shared_kv_across_executions() {
    let executor = WasmExecutor::new(WasmExecutorConfig::default())
        .expect("failed to create wasm executor");

    let bytes = load_test_component();

    // Shared KV across both executions.
    let kv: Arc<Mutex<dyn KvStore>> = Arc::new(Mutex::new(InMemoryKvStore::new()));

    let caps1 = Capabilities {
        kv: Arc::clone(&kv),
        config: Arc::new(MapConfig::new(HashMap::new())),
        log: Arc::new(TracingLogHost::new("exec-1".to_string(), "node-1".to_string())),
        http: Arc::new(ReqwestHttpHost::new(HttpPolicy::default())),
    };

    let caps2 = Capabilities {
        kv: Arc::clone(&kv),
        config: Arc::new(MapConfig::new(HashMap::new())),
        log: Arc::new(TracingLogHost::new("exec-1".to_string(), "node-2".to_string())),
        http: Arc::new(ReqwestHttpHost::new(HttpPolicy::default())),
    };

    // First task writes to KV (the test component does kv::set("last_input", &data)).
    let output1 = executor
        .execute(
            &bytes,
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
            &bytes,
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
    let executor = WasmExecutor::new(WasmExecutorConfig::default())
        .expect("failed to create wasm executor");

    let bytes = load_test_component();

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
            &bytes,
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
async fn test_component_caching() {
    let executor = WasmExecutor::new(WasmExecutorConfig::default())
        .expect("failed to create wasm executor");

    let bytes = load_test_component();

    // Execute twice with the same bytes — second call should use cached component.
    for i in 0..2 {
        let capabilities = make_capabilities();
        let output = executor
            .execute(
                &bytes,
                capabilities,
                TaskInput {
                    data: serde_json::json!({"iteration": i}),
                },
            )
            .await
            .expect("execution failed");

        assert_eq!(output.data["input"]["iteration"], i);
    }
}
