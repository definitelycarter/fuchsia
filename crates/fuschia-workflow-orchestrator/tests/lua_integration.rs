//! Integration tests for the workflow orchestrator using Lua as the runtime.
//!
//! These tests exercise the full orchestrator pipeline: graph traversal,
//! input resolution, error propagation, parallel execution, joins, and
//! cancellation — all without needing compiled wasm components.

use std::collections::HashMap;
use std::sync::Arc;

use fuschia_component_registry::InMemoryComponentRegistry;
use fuschia_config::RuntimeType as ConfigRuntimeType;
use fuschia_task_runtime::{RuntimeRegistry, RuntimeType};
use fuschia_task_runtime_lua::LuaExecutor;
use fuschia_workflow::{LockedComponent, Node, NodeType, Workflow};
use fuschia_workflow_orchestrator::{Orchestrator, OrchestratorConfig};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Lua scripts
// ---------------------------------------------------------------------------

/// Echo: returns input as output.
const ECHO_LUA: &str = r#"
function execute(ctx, data)
    return data
end
"#;

/// Fail: always errors.
const FAIL_LUA: &str = r#"
function execute(ctx, data)
    error("intentional failure")
end
"#;

/// Transform: wraps input in a transform envelope.
const TRANSFORM_LUA: &str = r#"
function execute(ctx, data)
    return '{"transformed": true, "original": ' .. data .. '}'
end
"#;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lua_task_node(node_id: &str, component_name: &str) -> (String, Node) {
    (
        node_id.to_string(),
        Node {
            node_id: node_id.to_string(),
            node_type: NodeType::Component(LockedComponent {
                name: component_name.to_string(),
                version: "1.0.0".to_string(),
                digest: "sha256:test".to_string(),
                task_name: "execute".to_string(),
                input_schema: serde_json::json!({}),
                runtime_type: ConfigRuntimeType::Lua,
            }),
            inputs: HashMap::new(),
            timeout_ms: None,
            max_retry_attempts: None,
            fail_workflow: false,
        },
    )
}

fn lua_task_node_with_inputs(
    node_id: &str,
    component_name: &str,
    inputs: HashMap<String, String>,
) -> (String, Node) {
    (
        node_id.to_string(),
        Node {
            node_id: node_id.to_string(),
            node_type: NodeType::Component(LockedComponent {
                name: component_name.to_string(),
                version: "1.0.0".to_string(),
                digest: "sha256:test".to_string(),
                task_name: "execute".to_string(),
                input_schema: serde_json::json!({}),
                runtime_type: ConfigRuntimeType::Lua,
            }),
            inputs,
            timeout_ms: None,
            max_retry_attempts: None,
            fail_workflow: false,
        },
    )
}

fn join_node(node_id: &str) -> (String, Node) {
    (
        node_id.to_string(),
        Node {
            node_id: node_id.to_string(),
            node_type: NodeType::Join {
                strategy: fuschia_config::JoinStrategy::All,
            },
            inputs: HashMap::new(),
            timeout_ms: None,
            max_retry_attempts: None,
            fail_workflow: false,
        },
    )
}

fn make_workflow(
    nodes: Vec<(String, Node)>,
    edges: Vec<(&str, &str)>,
) -> Workflow {
    Workflow {
        workflow_id: "test-workflow".to_string(),
        name: "Test Workflow".to_string(),
        nodes: nodes.into_iter().collect(),
        edges: edges
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
        timeout_ms: None,
        max_retry_attempts: None,
    }
}

fn make_orchestrator(
    workflow: Workflow,
    components: Vec<(&str, &[u8])>,
) -> Orchestrator {
    let mut registry = RuntimeRegistry::new();
    registry.register(RuntimeType::Lua, Arc::new(LuaExecutor::new()));

    let component_registry = Arc::new(InMemoryComponentRegistry::new());
    for (name, bytes) in components {
        component_registry.register(name, "1.0.0", bytes.to_vec());
    }

    Orchestrator::new(
        workflow,
        Arc::new(registry),
        component_registry,
        OrchestratorConfig::default(),
    )
    .expect("failed to create orchestrator")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// echo entry node → verify output matches payload.
#[tokio::test]
async fn test_single_node_success() {
    let mut inputs = HashMap::new();
    inputs.insert("msg".to_string(), "{{ msg }}".to_string());

    let workflow = make_workflow(
        vec![lua_task_node_with_inputs("A", "echo", inputs)],
        vec![],
    );

    let orch = make_orchestrator(workflow, vec![("echo", ECHO_LUA.as_bytes())]);

    let result = orch
        .invoke(serde_json::json!({"msg": "hello"}), CancellationToken::new())
        .await
        .expect("invoke failed");

    let a_output = &result.node_results["A"].output;
    assert_eq!(a_output["msg"], "hello");
}

/// A → B → C (linear chain), verify data flows through.
#[tokio::test]
async fn test_linear_chain() {
    let mut a_inputs = HashMap::new();
    a_inputs.insert("msg".to_string(), "{{ msg }}".to_string());

    let mut b_inputs = HashMap::new();
    b_inputs.insert("msg".to_string(), "{{ msg }}".to_string());

    let mut c_inputs = HashMap::new();
    c_inputs.insert("transformed".to_string(), "{{ transformed }}".to_string());

    let workflow = make_workflow(
        vec![
            lua_task_node_with_inputs("A", "echo", a_inputs),
            lua_task_node_with_inputs("B", "transform", b_inputs),
            lua_task_node_with_inputs("C", "echo", c_inputs),
        ],
        vec![("A", "B"), ("B", "C")],
    );

    let orch = make_orchestrator(
        workflow,
        vec![
            ("echo", ECHO_LUA.as_bytes()),
            ("transform", TRANSFORM_LUA.as_bytes()),
        ],
    );

    let result = orch
        .invoke(serde_json::json!({"msg": "hello"}), CancellationToken::new())
        .await
        .expect("invoke failed");

    // B wraps A's output: { transformed: true, original: <A's output> }
    let b_output = &result.node_results["B"].output;
    assert_eq!(b_output["transformed"], true);

    // C echoes B's transformed field (template renders bool as string)
    let c_output = &result.node_results["C"].output;
    assert_eq!(c_output["transformed"], "true");
}

/// [A, B] in parallel (both entry points), verify both run.
#[tokio::test]
async fn test_parallel_fan_out() {
    let mut a_inputs = HashMap::new();
    a_inputs.insert("fan".to_string(), "{{ fan }}".to_string());

    let mut b_inputs = HashMap::new();
    b_inputs.insert("fan".to_string(), "{{ fan }}".to_string());

    let workflow = make_workflow(
        vec![
            lua_task_node_with_inputs("A", "echo", a_inputs),
            lua_task_node_with_inputs("B", "echo", b_inputs),
        ],
        vec![],
    );

    let orch = make_orchestrator(workflow, vec![("echo", ECHO_LUA.as_bytes())]);

    let result = orch
        .invoke(serde_json::json!({"fan": "out"}), CancellationToken::new())
        .await
        .expect("invoke failed");

    assert!(result.node_results.contains_key("A"));
    assert!(result.node_results.contains_key("B"));
    assert_eq!(result.node_results["A"].output["fan"], "out");
    assert_eq!(result.node_results["B"].output["fan"], "out");
}

/// [A, B] → join → C, verify join merges and C receives it.
#[tokio::test]
async fn test_fan_out_join() {
    let workflow = make_workflow(
        vec![
            lua_task_node("A", "echo"),
            lua_task_node("B", "echo"),
            join_node("join"),
            lua_task_node("C", "echo"),
        ],
        vec![
            ("A", "join"),
            ("B", "join"),
            ("join", "C"),
        ],
    );

    let orch = make_orchestrator(workflow, vec![("echo", ECHO_LUA.as_bytes())]);

    let result = orch
        .invoke(serde_json::json!({"data": 1}), CancellationToken::new())
        .await
        .expect("invoke failed");

    // Join should have merged A and B outputs
    assert!(result.node_results.contains_key("join"));
    // C receives the join output
    assert!(result.node_results.contains_key("C"));
}

/// A (fails) → B, verify B never runs and workflow errors.
#[tokio::test]
async fn test_task_failure_stops_workflow() {
    let workflow = make_workflow(
        vec![
            lua_task_node("A", "fail"),
            lua_task_node("B", "echo"),
        ],
        vec![("A", "B")],
    );

    let orch = make_orchestrator(
        workflow,
        vec![
            ("fail", FAIL_LUA.as_bytes()),
            ("echo", ECHO_LUA.as_bytes()),
        ],
    );

    let result = orch
        .invoke(serde_json::json!({}), CancellationToken::new())
        .await;

    assert!(result.is_err(), "workflow should have failed");
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("task execution failed"),
        "expected TaskExecution error, got: {msg}"
    );
}

/// [A (ok), B (fails)] → C, verify workflow fails.
#[tokio::test]
async fn test_failure_in_parallel_batch() {
    let workflow = make_workflow(
        vec![
            lua_task_node("A", "echo"),
            lua_task_node("B", "fail"),
            lua_task_node("C", "echo"),
        ],
        vec![("A", "C"), ("B", "C")],
    );

    let orch = make_orchestrator(
        workflow,
        vec![
            ("echo", ECHO_LUA.as_bytes()),
            ("fail", FAIL_LUA.as_bytes()),
        ],
    );

    let result = orch
        .invoke(serde_json::json!({}), CancellationToken::new())
        .await;

    assert!(result.is_err(), "workflow should have failed due to B");
}

/// A → B (with template input), verify template resolves.
#[tokio::test]
async fn test_input_template_resolution() {
    // A (entry point) forwards the payload's `msg` field
    let mut a_inputs = HashMap::new();
    a_inputs.insert("msg".to_string(), "{{ msg }}".to_string());

    // B references A's output field (single upstream → direct field access)
    let mut b_inputs = HashMap::new();
    b_inputs.insert("greeting".to_string(), "{{ msg }}".to_string());

    let workflow = make_workflow(
        vec![
            lua_task_node_with_inputs("A", "echo", a_inputs),
            lua_task_node_with_inputs("B", "echo", b_inputs),
        ],
        vec![("A", "B")],
    );

    let orch = make_orchestrator(workflow, vec![("echo", ECHO_LUA.as_bytes())]);

    let result = orch
        .invoke(
            serde_json::json!({"msg": "hello world"}),
            CancellationToken::new(),
        )
        .await
        .expect("invoke failed");

    // B's resolved_input should have the template resolved
    let b_resolved = &result.node_results["B"].resolved_input;
    assert_eq!(b_resolved["greeting"], "hello world");
}

/// A (pre-cancelled), verify Cancelled error.
#[tokio::test]
async fn test_cancellation() {
    // Use a script with a busy loop that can be interrupted by the cancel check.
    // Note: Lua execution is synchronous, so cancellation is checked between waves.
    // We test that the cancel token is checked before execution starts.
    let workflow = make_workflow(
        vec![lua_task_node("A", "echo")],
        vec![],
    );

    let orch = make_orchestrator(workflow, vec![("echo", ECHO_LUA.as_bytes())]);

    // Pre-cancel the token
    let cancel = CancellationToken::new();
    cancel.cancel();

    let result = orch.invoke(serde_json::json!({}), cancel).await;

    assert!(result.is_err(), "should have been cancelled");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("cancelled"),
        "expected cancellation error, got: {msg}"
    );
}

/// A with empty payload, verify it still works.
#[tokio::test]
async fn test_empty_input() {
    let workflow = make_workflow(
        vec![lua_task_node("A", "echo")],
        vec![],
    );

    let orch = make_orchestrator(workflow, vec![("echo", ECHO_LUA.as_bytes())]);

    let result = orch
        .invoke(serde_json::json!({}), CancellationToken::new())
        .await
        .expect("invoke failed");

    assert!(result.node_results.contains_key("A"));
}

/// [10 parallel entry tasks] → join, verify all run and merge.
#[tokio::test]
async fn test_large_fan_out() {
    let task_ids: Vec<String> = (0..10).map(|i| format!("task_{i}")).collect();
    let mut nodes: Vec<(String, Node)> =
        task_ids.iter().map(|id| lua_task_node(id, "echo")).collect();
    nodes.push(join_node("join"));

    let edge_pairs: Vec<(String, String)> = task_ids
        .iter()
        .map(|id| (id.clone(), "join".to_string()))
        .collect();

    let workflow = Workflow {
        workflow_id: "fan-out-test".to_string(),
        name: "Fan-out Test".to_string(),
        nodes: nodes.into_iter().collect(),
        edges: edge_pairs,
        timeout_ms: None,
        max_retry_attempts: None,
    };

    let mut registry = RuntimeRegistry::new();
    registry.register(RuntimeType::Lua, Arc::new(LuaExecutor::new()));

    let component_registry = Arc::new(InMemoryComponentRegistry::new());
    component_registry.register("echo", "1.0.0", ECHO_LUA.as_bytes().to_vec());

    let orch = Orchestrator::new(
        workflow,
        Arc::new(registry),
        component_registry,
        OrchestratorConfig::default(),
    )
    .expect("failed to create orchestrator");

    let result = orch
        .invoke(serde_json::json!({"n": 10}), CancellationToken::new())
        .await
        .expect("invoke failed");

    // All 10 tasks + join = 11 nodes
    assert_eq!(result.node_results.len(), 11);
    for id in &task_ids {
        assert!(
            result.node_results.contains_key(id),
            "missing result for {id}"
        );
    }
    assert!(result.node_results.contains_key("join"));
}
