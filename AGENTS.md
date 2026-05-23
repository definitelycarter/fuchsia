# Fuchsia

Fuchsia is a workflow engine similar to n8n, built on WebAssembly components using WIT (WebAssembly Interface Types). Each workflow node is a component (Wasm or Lua) with explicitly defined capabilities. Workflows are invoked manually with a JSON payload that flows into graph entry points.

## Documentation

- [docs/DESIGN.md](./docs/DESIGN.md) - Architecture and design decisions
- [docs/USE_CASES.md](./docs/USE_CASES.md) - Example workflows with diagrams and JSON definitions
- [docs/ANALYSIS.md](./docs/ANALYSIS.md) - Codebase analysis: bugs, dead code, design issues

## Project Structure

- `src/` - `fuchsia` binary crate (CLI for running workflows and individual nodes)
- `crates/` - Workspace member crates
  - `fuchsia-config` - Serializable workflow configuration types (`WorkflowDef`, `NodeDef`, `Edge`, `NodeType`). Deserialized from JSON files or database storage before being resolved into a locked workflow.
  - `fuchsia-workflow` - Locked/resolved workflow representation. Validated DAG with components referenced by name, version, and digest. Includes the `Graph` traversal helper (entry points, join points, upstream/downstream lookup).
  - `fuchsia-component-registry` - Component registry system. Manages installed components with manifests containing name, version, description, digest, capabilities (`allowed_hosts`, `allowed_paths`), and a HashMap of exported tasks. `FsComponentRegistry` stores components in `~/.fuchsia/components/` with a npm-like directory structure; `InMemoryComponentRegistry` is for tests.
  - `fuchsia-resolver` - Transforms `WorkflowDef` (config) into `Workflow` (locked). Validates the graph is a DAG (no cycles, no orphan node references), resolves component references via the registry, and recursively resolves loop nodes.
  - `fuchsia-artifact` - Artifact storage trait and implementations. Async streaming interface for storing/retrieving binary artifacts. Includes `FsStore` for local filesystem storage.
  - `fuchsia-host-config` - `ConfigHost` trait and `MapConfig` for read-only configuration lookup exposed to components.
  - `fuchsia-host-kv` - `KvStore` trait for execution-scoped key-value storage exposed to components.
  - `fuchsia-host-log` - `LogHost` trait for routing component logs into the host's tracing infrastructure.
  - `fuchsia-host-http` - HTTP capability for components with `allowed_hosts` enforcement.
  - `fuchsia-task-runtime` - Runtime-agnostic task execution interface. Defines `NodeExecutor`, `Capabilities`, `TaskInput`, `TaskOutput`, and `RuntimeRegistry`, which routes execution to the appropriate backend by `RuntimeType` (Wasm or Lua).
  - `fuchsia-task-runtime-wasm` - `WasmExecutor` — Wasmtime-backed component executor. Implements `NodeExecutor` using `fuchsia-world` bindings.
  - `fuchsia-task-runtime-lua` - `LuaExecutor` — Lua-backed task executor. Implements `NodeExecutor` so Lua scripts can be used wherever a Wasm component is expected.
  - `fuchsia-workflow-orchestrator` - Workflow execution engine. `Orchestrator::invoke(payload, cancel)` runs a workflow to completion; `invoke_node` runs a single node for debugging. Handles DAG traversal, wave-based parallel scheduling, minijinja-based input resolution, type coercion, and cancellation. Delegates all component execution to `RuntimeRegistry`. Entry-point nodes (nodes with no incoming edges) receive the payload as a virtual single upstream.
  - `fuchsia-world` - Wasmtime bindgen host world. Uses `wasmtime::component::bindgen!` to generate Rust bindings from the WIT interfaces.
- `wit/` - WebAssembly Interface Type (WIT) definitions
  - `world.wit` - Platform world with shared imports (`kv`, `config`, `log`); `task-component` world extends platform and exports the task interface.
  - `deps/` - WIT package dependencies
    - `fuchsia-task/task.wit` - Task interface with context (execution-id, node-id, task-id) and `execute` function
    - `fuchsia-kv/kv.wit` - Key-value store host import for component state persistence
    - `fuchsia-config/config.wit` - Config host import for lazy configuration lookup
    - `fuchsia-log/log.wit` - Logging interface that routes to the host's tracing layer
    - `wasi_http@0.2.0.wit`, `wasi_io@0.2.0.wit`, `wasi_clocks@0.2.0.wit`, etc. - WASI dependencies
- `test-components/` - Workspace-excluded crates that compile to Wasm components for integration tests (`test-task-component`).
- `examples/` - Sample workflow JSON files.
- `docs/` - Design documentation.

## Development

### Building

```bash
cargo build
```

### Testing

```bash
cargo test --workspace
```

### Formatting

```bash
cargo fmt
```

## Guidelines

- Follow Rust idioms and best practices
- Use `cargo fmt` before committing
- Ensure all tests pass with `cargo test --workspace`
- Add tests for new functionality
- Do not automatically commit or push to this repository - wait for explicit user approval
- Avoid `clone()` in production code - provide justification if proposing it (acceptable in tests)
- Avoid `unwrap()`, `expect()`, and other panic-prone error handling in production code (acceptable in tests)
- Avoid `.ok()` to silently discard errors in production code — propagate errors with `?` or `map_err` instead (acceptable in tests and in `sort_by` closures where returning `Result` is not possible)
