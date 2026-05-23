# WebAssembly Runtime

The WebAssembly runtime executes tasks as WASI Preview 2 components via [wasmtime](https://wasmtime.dev/). It is the most mature runtime backend.

**Crate**: `fuchsia-task-runtime-wasm`

## Why Wasmtime

- Native Rust implementation
- Best WIT/component model support
- Developed by the Bytecode Alliance, which drives the WIT and component model specifications

## `WasmExecutor`

Implements `NodeExecutor`. Owns the wasmtime `Engine` and an internal component cache.

```rust
let executor = WasmExecutor::new(WasmExecutorConfig::default())?;

let output = executor.execute(&wasm_bytes, capabilities, input).await?;
```

### Internal Architecture

| Concern | How |
|---------|-----|
| Engine | Single `wasmtime::Engine` created at construction, shared across all executions |
| Compilation cache | `RwLock<HashMap<u64, Component>>` keyed by byte hash. Compiled once, reused. |
| Instance isolation | Fresh `Store` + `Instance` per `execute()` call. No wasm memory shared between calls. |
| Host capabilities | `WasmTaskState` holds `Arc` clones of `Capabilities`. WIT `Host` trait impls delegate to these. |

### Execution Flow

1. **Compile or cache** — hash the bytes, check cache, compile `Component` if miss
2. **Create fresh store** — `WasmTaskState::from_capabilities(&capabilities)` + `Store::new()`
3. **Link imports** — WASI imports + fuchsia host imports (kv, config, log)
4. **Instantiate** — `TaskComponent::instantiate_async()`
5. **Call** — `instance.fuchsia_task_task().call_execute()`
6. **Return** — parse JSON output into `TaskOutput`

## Host Capability Wiring

WIT imports map to shared host capability implementations via thin glue in `WasmTaskState`:

| WIT Import | Capability | Glue |
|------------|-----------|------|
| `fuchsia:kv/kv` | `Arc<Mutex<dyn KvStore>>` | `futures::executor::block_on(kv.lock().await.get())` |
| `fuchsia:config/config` | `Arc<dyn ConfigHost>` | `self.config.get(&key)` |
| `fuchsia:log/log` | `Arc<dyn LogHost>` | `self.log.log(level, &message)` |

The `WasmTaskState` struct holds `Arc` clones of the shared capabilities. Each WIT host trait impl is a few lines that delegate to the corresponding capability.

## WIT Worlds

Components implement the `task-component` world:

### task-component

```wit
world task-component {
    include platform;
    export fuchsia:task/task@0.1.0;
}
```

### platform (shared imports)

```wit
world platform {
    import fuchsia:kv/kv;
    import fuchsia:config/config;
    import fuchsia:log/log;
}
```

## Timeout Enforcement

Uses wasmtime's epoch-based interruption:

1. Engine configured with `epoch_interruption(true)`
2. Store gets `set_epoch_deadline(N)` before calling wasm
3. Background task increments engine epoch after timeout duration
4. Wasm traps with `EpochInterruption` if still running

## Configuration

```rust
pub struct WasmExecutorConfig {
    pub epoch_interruption: bool,     // default: true
    pub default_epoch_deadline: u64,  // default: u64::MAX
}
```

## Async Execution

The WebAssembly component model doesn't have native async yet. WASIp3 will add `stream` and `future` types. Until then, wasm calls are run on tokio's async runtime with wasmtime's async support enabled.
