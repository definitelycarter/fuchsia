# Runtimes

A runtime is a VM backend that can execute component code. The orchestrator treats all runtimes identically through a shared trait — it doesn't know or care which VM is running a given node.

## The `NodeExecutor` Trait

```rust
pub trait NodeExecutor: Send + Sync {
    fn execute<'a>(
        &'a self,
        bytes: &'a [u8],
        capabilities: Capabilities,
        input: TaskInput,
    ) -> Pin<Box<dyn Future<Output = Result<TaskOutput, ExecutorError>> + Send + 'a>>;
}
```

### Contract

- **Input**: raw bytes (wasm binary, Lua source, JS source) + shared capabilities + structured JSON input
- **Output**: structured JSON output
- **Lifecycle**: the executor owns its caching strategy internally. Wasmtime compiles once and caches `Component`. Lua reads source each time (or caches bytecode). The caller never sees these details.
- **Isolation**: VM instances (wasm memory, lua state) are created fresh per invocation. No state leakage between calls.

## Capabilities

Capabilities are constructed **per-node**. Each node gets its own `Capabilities` with policies from its component manifest. Some backing resources are workflow-scoped (e.g. KV, so nodes can communicate), while others are node-specific (e.g. HTTP policy, config, log context).

```rust
#[derive(Clone)]
pub struct Capabilities {
    pub kv: Arc<Mutex<dyn KvStore>>,
    pub config: Arc<dyn ConfigHost>,
    pub log: Arc<dyn LogHost>,
    pub http: Arc<dyn HttpHost>,
}
```

Each field points to a **shared host capability implementation** — there is one `KvStore` implementation, one `HttpHost` implementation, etc. Runtimes do not re-implement these. They write thin glue to wire their VM's calling convention to the shared implementation:

- **Wasmtime**: WIT imports → calls `capabilities.kv.lock().await.get()`
- **Lua**: global function `kv_get(key)` → calls `capabilities.kv.lock().await.get()`
- **JS**: module binding `kv.get(key)` → calls `capabilities.kv.lock().await.get()`

See [Host Capabilities](./host-capabilities.md) for details on each capability.

## Runtime Registry

The `RuntimeRegistry` maps `RuntimeType` → `Arc<dyn NodeExecutor>`. Runtime types are feature-gated:

```rust
pub enum RuntimeType {
    #[cfg(feature = "wasm")]
    Wasm,
    #[cfg(feature = "lua")]
    Lua,
}
```

Executors are registered at startup:

```rust
let mut registry = RuntimeRegistry::new();
registry.register(RuntimeType::Wasm, Arc::new(WasmExecutor::new(config)?));
registry.register(RuntimeType::Lua, Arc::new(LuaExecutor::new()));
```

The orchestrator calls `registry.execute(runtime_type, bytes, capabilities, input)` for each node. It never touches wasmtime, mlua, or any VM directly.

## Runtime Implementations

| Runtime | Crate | VM | Status |
|---------|-------|----|--------|
| [WebAssembly](../runtimes/wasm.md) | `fuschia-task-runtime-wasm` | wasmtime | Done |
| [Lua](../runtimes/lua.md) | `fuschia-task-runtime-lua` | mlua | Done |
| [JavaScript](../runtimes/js.md) | `fuschia-task-runtime-js` | TBD | Future |
