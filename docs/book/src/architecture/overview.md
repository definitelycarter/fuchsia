# Architecture Overview

## System Diagram

```mermaid
graph TD
    JSON["Workflow JSON"] --> Config["fuchsia-config<br/>Deserialize workflow definition"]
    Config --> Resolver["fuchsia-resolver<br/>Validate DAG, resolve components"]
    Resolver --> Workflow["fuchsia-workflow<br/>Locked workflow + graph traversal"]
    Workflow --> Orchestrator

    subgraph Orchestrator["Orchestrator (fuchsia-workflow-orchestrator)"]
        direction TB
        Desc["Graph traversal, input resolution, scheduling<br/>Provisions nodes via runtime backends"]
        Trait["NodeExecutor trait<br/>bytes + capabilities + input → output"]
        Desc --> Trait
        Trait --> Wasm["Wasm Runtime"]
        Trait --> Lua["Lua Runtime"]
        Trait --> JS["JS Runtime"]
    end

    Wasm --> Capabilities
    Lua --> Capabilities
    JS --> Capabilities

    subgraph Capabilities["Host Capabilities (shared)"]
        KV["fuchsia-host-kv<br/>KV store"]
        HTTP["fuchsia-host-http<br/>HTTP client + policy"]
        ConfigHost["fuchsia-host-config<br/>Config lookup"]
        Log["fuchsia-host-log<br/>Logging / OTel"]
        FS["fuchsia-host-fs<br/>Filesystem + policy"]
    end
```

## Dependency Graph

```mermaid
graph BT
    CLI["fuchsia (CLI)"] --> Orchestrator
    Orchestrator["fuchsia-workflow-orchestrator"] --> WasmRT & LuaRT
    Orchestrator --> FConfig["fuchsia-config"]
    Orchestrator --> FRegistry["fuchsia-component-registry"]
    Orchestrator --> FWorkflow["fuchsia-workflow"]

    WasmRT["fuchsia-task-runtime-wasm"] --> Trait
    LuaRT["fuchsia-task-runtime-lua"] --> Trait

    Trait["fuchsia-task-runtime<br/>(trait)"] --> HostKV["fuchsia-host-kv"]
    Trait --> HostConfig["fuchsia-host-config"]
    Trait --> HostLog["fuchsia-host-log"]
    Trait --> HostHTTP["fuchsia-host-http"]
```

## Key Principles

- **Bytes in, JSON out** — The runtime trait takes raw bytes (wasm binary, Lua source, JS source) and structured input, returns structured output. Each runtime interprets the bytes in its own way.
- **One implementation per host capability** — KV, HTTP, logging, config, filesystem are each implemented in a single crate. Runtimes write thin glue to wire their VM's FFI to these shared implementations.
- **Orchestrator is runtime-agnostic** — The orchestrator resolves workflows into nodes, provisions them using whatever runtime is registered, and executes them. It never touches wasmtime, mlua, or any VM directly.
- **Runtimes own their lifecycle** — Each runtime manages its own caching, compilation, and instance creation internally. Wasmtime compiles once and caches `Component`. Lua reads the source. The orchestrator doesn't care.
- **Manual invocation with payload** — Workflows are kicked off by calling `Orchestrator::invoke(payload, cancel)` with a JSON payload. Nodes with no incoming edges (entry points) receive the payload as their upstream context.
