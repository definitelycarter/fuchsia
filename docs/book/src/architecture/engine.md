# Orchestrator

The orchestrator (`fuschia-workflow-orchestrator`) owns a workflow graph and walks it wave-by-wave, resolving inputs and dispatching node execution to runtime backends. It has no knowledge of wasmtime, Lua, or any specific VM.

## Responsibilities

- **Graph traversal** — Find ready nodes (all upstream dependencies complete), execute them, repeat
- **Parallel scheduling** — Independent branches spawn as concurrent tokio tasks
- **Input resolution** — Render minijinja templates against upstream data, coerce types per schema
- **Runtime dispatch** — Hand off node execution to a `NodeExecutor` implementation via `RuntimeRegistry`
- **Payload injection** — Nodes with no incoming edges (entry points) receive the invocation payload as if it came from a single virtual upstream node
- **Cancellation** — Propagate `CancellationToken` to all running nodes
- **Validation** — Enforce at least one entry point, no orphan nodes

## Execution Flow

```
invoke(payload, cancel)
  │
  ├─ validate_workflow()
  │    • at least one entry-point node (no incoming edges)
  │    • no orphan nodes
  │
  └─ run_execution_loop()
       │
       • seed entry-point nodes with `payload` as their upstream data
       │
       loop:
         ├─ find_ready_nodes(completed)
         │    • nodes where all upstream are in completed map
         │    • entry-point nodes are ready immediately
         │
         ├─ execute_ready_nodes(ready)
         │    for each ready node:
         │      ├─ gather upstream data (payload for entry points)
         │      ├─ resolve_inputs() — minijinja templates → strings
         │      ├─ coerce_inputs() — strings → typed JSON per schema
         │      └─ spawn tokio task → registry.execute(rt, bytes, caps, input)
         │
         ├─ await all tasks (or cancellation)
         │
         └─ store results, find next wave
```

For single-node debugging, `invoke_node(node_id, payload, cancel)` runs one node in isolation. The payload is wrapped under a `_payload` key, so templates reference fields as `{{ _payload.field }}`.

## What Lives in the Orchestrator vs the Runtime

| Concern | Orchestrator | Runtime |
|---------|-------------|---------|
| Graph traversal | Yes | No |
| Input resolution (minijinja) | Yes | No |
| Type coercion (schema-based) | Yes | No |
| Parallel scheduling | Yes | No |
| Cancellation propagation | Yes | Receives token |
| VM instantiation | No | Yes |
| Artifact compilation/caching | No | Yes |
| Host capability wiring | No | Yes |
| Timeout enforcement | Passes timeout | Enforces it |

## CLI Integration

The CLI (`src/main.rs`) creates an `Orchestrator` directly:

```rust
let mut runtime_registry = RuntimeRegistry::new();
runtime_registry.register(RuntimeType::Wasm, Arc::new(WasmExecutor::new(config)?));

let orchestrator = Orchestrator::new(
    workflow,
    Arc::new(runtime_registry),
    component_registry,
    OrchestratorConfig::default(),
)?;

orchestrator.invoke(payload, cancel).await?;
```
