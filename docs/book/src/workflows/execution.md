# Execution

The engine executes workflows using a wave-based approach: find ready nodes, execute them in parallel, collect results, repeat.

## Execution Loop

```
invoke(payload, cancel)
  │
  ├─ validate_workflow()
  │
  ├─ seed entry-point nodes with `payload` as upstream data
  │
  └─ loop:
       ├─ find_ready_nodes(completed)
       ├─ execute_ready_nodes(ready) → spawn tokio tasks
       ├─ await all tasks (or cancellation)
       └─ store results → next wave
```

## Parallel Execution

Parallelism emerges from graph structure. Multiple entry-point nodes (nodes with no incoming edges) all receive the payload and execute concurrently. When a node has multiple downstream edges, the downstream nodes become ready simultaneously and execute as concurrent tokio tasks.

```
[A] → [D]
[B] → [D]
[C] → [D]
```

If A, B, and C are all entry points, they execute in parallel against the invocation payload. D waits for all three.

## Ready Node Detection

A node is "ready" when all its upstream nodes are in the completed map:

```rust
fn find_ready_nodes(completed: &HashMap<String, NodeResult>) -> Vec<String> {
    graph.all_nodes()
        .filter(|id| !completed.contains_key(id))
        .filter(|id| graph.upstream(id).iter().all(|up| completed.contains_key(up)))
        .collect()
}
```

## Cancellation

Two levels of cancellation:

- **Workflow-level**: `CancellationToken` propagated to all running nodes. When cancelled, the execution loop exits.
- **Node-level**: Epoch interruption (wasmtime) or VM-specific timeout for in-flight execution.

Each workflow execution gets a child cancellation token, so cancelling one execution doesn't affect others.

## Join Nodes

Join nodes synchronize parallel branches. They are non-component nodes that pass through merged upstream data:

- Input: map of all upstream node outputs, keyed by node ID
- Output: same map (pass-through)

Join strategies (`All` / `Any`) control whether the workflow proceeds on partial success.

## Loop Nodes

Loop nodes iterate over a collection and execute a nested workflow per item:

- **Sequential**: one item at a time
- **Parallel**: concurrent execution with configurable concurrency limit

Failure modes:
- `stop_and_fail` — stop on first failure, fail the loop
- `stop_with_errors` — stop on first failure, complete with errors
- `continue` — log errors, continue processing remaining items
