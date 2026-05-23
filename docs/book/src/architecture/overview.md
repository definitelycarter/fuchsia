# Architecture Overview

## System Diagram

```mermaid
graph TD
    JSON["Workflow JSON"] --> Config["fuschia-config<br/>Deserialize workflow definition"]
    Config --> Resolver["fuschia-resolver<br/>Validate DAG, resolve components"]
    Resolver --> Workflow["fuschia-workflow<br/>Locked workflow + graph traversal"]
    Workflow --> Orchestrator

    subgraph Orchestrator["Orchestrator (fuschia-workflow-orchestrator)"]
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
        KV["fuschia-host-kv<br/>KV store"]
        HTTP["fuschia-host-http<br/>HTTP client + policy"]
        ConfigHost["fuschia-host-config<br/>Config lookup"]
        Log["fuschia-host-log<br/>Logging / OTel"]
        FS["fuschia-host-fs<br/>Filesystem + policy"]
    end
```

## Dependency Graph

```mermaid
graph BT
    CLI["fuschia (CLI)"] --> Orchestrator
    Orchestrator["fuschia-workflow-orchestrator"] --> WasmRT & LuaRT
    Orchestrator --> FConfig["fuschia-config"]
    Orchestrator --> FRegistry["fuschia-component-registry"]
    Orchestrator --> FWorkflow["fuschia-workflow"]

    WasmRT["fuschia-task-runtime-wasm"] --> Trait
    LuaRT["fuschia-task-runtime-lua"] --> Trait

    Trait["fuschia-task-runtime<br/>(trait)"] --> HostKV["fuschia-host-kv"]
    Trait --> HostConfig["fuschia-host-config"]
    Trait --> HostLog["fuschia-host-log"]
    Trait --> HostHTTP["fuschia-host-http"]
```

## Key Principles

- **Bytes in, JSON out** — The runtime trait takes raw bytes (wasm binary, Lua source, JS source) and structured input, returns structured output. Each runtime interprets the bytes in its own way.
- **One implementation per host capability** — KV, HTTP, logging, config, filesystem are each implemented in a single crate. Runtimes write thin glue to wire their VM's FFI to these shared implementations.
- **Orchestrator is runtime-agnostic** — The orchestrator resolves workflows into nodes, provisions them using whatever runtime is registered, and executes them. It never touches wasmtime, mlua, or any VM directly.
- **Runtimes own their lifecycle** — Each runtime manages its own caching, compilation, and instance creation internally. Wasmtime compiles once and caches `Component`. Lua reads the source. The orchestrator doesn't care.
- **Manual invocation with payload** — Workflows are kicked off by calling `Orchestrator::invoke(payload, cancel)` with a JSON payload. Nodes with no incoming edges (entry points) receive the payload as their upstream context.
