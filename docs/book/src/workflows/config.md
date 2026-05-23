# Configuration

Workflows are defined as JSON files containing nodes, edges, and configuration. The `fuchsia-config` crate provides the serializable types.

## WorkflowDef

The top-level workflow definition:

```json
{
  "workflow_id": "wf_simple_etl",
  "name": "Simple ETL Pipeline",
  "timeout_ms": 30000,
  "max_retry_attempts": 3,
  "retry_backoff": "exponential",
  "retry_initial_delay_ms": 1000,
  "nodes": [...],
  "edges": [...]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `workflow_id` | string | Unique identifier |
| `name` | string | Human-readable name |
| `timeout_ms` | u64? | Default timeout for all nodes |
| `max_retry_attempts` | u32? | Default retry count for all nodes |
| `retry_backoff` | enum? | `constant`, `linear`, or `exponential` |
| `retry_initial_delay_ms` | u64? | Initial retry delay |
| `nodes` | NodeDef[] | Node definitions |
| `edges` | Edge[] | Connections between nodes |

## NodeDef

Each node declares its type, component reference, and input mappings:

```json
{
  "node_id": "fetch_data",
  "type": "component",
  "component": "my-org/http-fetch",
  "version": "1.0.0",
  "task_name": "fetch",
  "inputs": {
    "url": "{{ api_url }}",
    "method": "GET"
  },
  "timeout_ms": 10000,
  "max_retry_attempts": 2,
  "fail_workflow": true
}
```

Node-level `timeout_ms` and `max_retry_attempts` override workflow defaults.

## Node Types

### Component

Executes a task exported by a wasm component (or Lua/JS script):

```json
{
  "node_id": "transform",
  "type": "component",
  "component": "json-transform",
  "task_name": "transform",
  "inputs": { "data": "{{ body }}" }
}
```

### Join

Synchronizes parallel branches:

```json
{
  "node_id": "sync",
  "type": "join",
  "join_strategy": "All"
}
```

Strategies: `All` (all must succeed) or `Any` (at least one must succeed).

### Loop

Iterates over a collection, executing a nested workflow per item:

```json
{
  "node_id": "process_items",
  "type": "loop",
  "execution_mode": "parallel",
  "concurrency": 5,
  "failure_mode": "continue",
  "nodes": [...],
  "edges": [...]
}
```

## Edges

Connections between nodes:

```json
{
  "edges": [
    { "from": "fetch_data", "to": "transform" },
    { "from": "transform", "to": "save" }
  ]
}
```

## Entry Points

Workflows form arbitrary DAGs. Any node with no incoming edges is an **entry point**. When the orchestrator is invoked with a JSON payload, the payload is delivered to every entry point as if it had come from a single virtual upstream node. Templates on entry-point nodes reference payload fields directly:

```json
{
  "node_id": "process",
  "type": "component",
  "component": "my-org/printer",
  "task_name": "print",
  "inputs": { "message": "{{ msg }}" }
}
```

Invoking with `{ "msg": "hello" }` would render `message` as `"hello"`.

## Input Values

All input values are template strings processed by minijinja at runtime. See [Input Resolution](./input-resolution.md).

```json
{
  "inputs": {
    "recipient": "{{ email }}",
    "count": "{{ items | length }}",
    "greeting": "Hello {{ name | title }}!"
  }
}
```
