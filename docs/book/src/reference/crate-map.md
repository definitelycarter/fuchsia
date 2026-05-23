# Crate Map

## Current Crates

### Configuration & Data Model

| Crate | Description | Dependencies |
|-------|-------------|--------------|
| `fuchsia-config` | Serializable workflow configuration (JSON). WorkflowDef, NodeDef, edges, RuntimeType. | serde |
| `fuchsia-workflow` | Locked/resolved workflow with graph traversal. Validated DAG. | fuchsia-config |
| `fuchsia-component-registry` | Component manifest, registry trait, filesystem registry, in-memory test registry. | fuchsia-config |
| `fuchsia-resolver` | Transform WorkflowDef → Workflow. Validate graph, resolve components. | fuchsia-config, fuchsia-workflow, fuchsia-component-registry |
| `fuchsia-artifact` | Artifact storage trait + FsStore implementation. | — |
| `fuchsia-world` | Wasmtime bindgen host world. Generates Rust bindings from WIT. | wasmtime |

### Host Capabilities

| Crate | Description | Dependencies |
|-------|-------------|--------------|
| `fuchsia-host-kv` | `KvStore` trait + `InMemoryKvStore`. Execution-scoped key-value storage. | — |
| `fuchsia-host-config` | `ConfigHost` trait + `MapConfig`. Read-only config lookup. | — |
| `fuchsia-host-log` | `LogHost` trait + `TracingLogHost`. Routes logs to tracing with execution context. | tracing |
| `fuchsia-host-http` | `HttpHost` trait + `ReqwestHttpHost` + `HttpPolicy`. HTTP with host allowlist enforcement. | reqwest |

### Runtime Abstraction

| Crate | Description | Dependencies | Status |
|-------|-------------|--------------|--------|
| `fuchsia-task-runtime` | `NodeExecutor` trait, `Capabilities` struct, `RuntimeRegistry`, `RuntimeType` enum. Feature-gated variants (`wasm`, `lua`). | fuchsia-host-* | Done |
| `fuchsia-task-runtime-wasm` | `WasmExecutor`: wasmtime `NodeExecutor` impl. Owns engine + component cache. Fresh VM instance per execution, shared capabilities. | fuchsia-task-runtime, wasmtime | Done |
| `fuchsia-task-runtime-lua` | `LuaExecutor`: mlua `NodeExecutor` impl. Executes Lua scripts with `execute(ctx, data)` function contract. | fuchsia-task-runtime, mlua | Done |

### Orchestration

| Crate | Description | Dependencies |
|-------|-------------|--------------|
| `fuchsia-workflow-orchestrator` | Workflow execution: DAG traversal, wave-based scheduling, input resolution, runtime dispatch. Fully runtime-agnostic — delegates all execution to `RuntimeRegistry`. | fuchsia-workflow, fuchsia-task-runtime, fuchsia-host-*, fuchsia-component-registry |

### Planned

| Crate | Description |
|-------|-------------|
| `fuchsia-host-fs` | Filesystem access + `FsPolicy` (allowed_paths) |
| `fuchsia-task-runtime-js` | JavaScript `NodeExecutor` impl |

## Dependency Flow

```mermaid
graph TD
    HostKV["fuchsia-host-kv"] --> TaskRT["fuchsia-task-runtime<br/>(trait + registry)"]
    HostConfig["fuchsia-host-config"] --> TaskRT
    HostLog["fuchsia-host-log"] --> TaskRT
    HostHTTP["fuchsia-host-http"] --> TaskRT

    TaskRT --> WasmRT["fuchsia-task-runtime-wasm"]
    TaskRT --> LuaRT["fuchsia-task-runtime-lua"]

    Config["fuchsia-config"] --> Registry["fuchsia-component-registry"]
    Config --> Resolver["fuchsia-resolver"]
    Resolver --> Workflow["fuchsia-workflow"]

    WasmRT --> Orchestrator["fuchsia-workflow-orchestrator"]
    LuaRT --> Orchestrator
    Registry --> Orchestrator
    Workflow --> Orchestrator

    Orchestrator --> CLI["fuchsia (CLI)"]
```

## Removed Crates

The following crates were removed during the runtime abstraction refactor:

| Crate | Reason |
|-------|--------|
| `fuchsia-runtime` | Replaced by `fuchsia-workflow-orchestrator` |
| `fuchsia-engine` | `WorkflowRunner` wrapper was unused; CLI uses orchestrator directly |
| `fuchsia-task` | Superseded by `fuchsia-task-runtime` |
| `fuchsia-task-host` | Superseded by `fuchsia-task-runtime-wasm` |
| `fuchsia-host` | Shared wasmtime infra; replaced by per-capability crates (host-kv, host-config, etc.) |
