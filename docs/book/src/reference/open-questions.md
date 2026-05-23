# Open Questions

Unresolved design decisions and areas for future exploration.

## Runtime Abstraction

| Question | Context |
|----------|---------|
| Byte source | Should executors receive bytes directly, or a path + option for bytes? Path is useful for wasmtime's `Component::from_file` optimization. Bytes-from-memory is useful for tests. |
| Runtime field in manifest | Should component manifests gain a `runtime: "wasm" | "lua"` field for dispatch? |

## Component System

| Question | Context |
|----------|---------|
| HTTP outbound filtering | How to enforce `allowed_hosts` with `wasmtime-wasi-http`? Custom `WasiHttpView` wrapper or implement own handler? |
| Shared Engine ownership | Should `fuchsia-host` own the Engine singleton, or should the caller create and pass it in? |
| KV store value types | Should `kv.wit` support complex types (json, number, bool, object) or just strings? |

## Execution

| Question | Context |
|----------|---------|
| Loop item injection | How does `{ "item": {...}, "index": 0 }` get passed to nested workflow inputs? |
| Join node output shape | What's the output — aggregated map of branch outputs? Pass-through? |
| Distributed execution model | `fuchsia start` daemon mode: init container pulls components at deploy time, pods wait for messages from broker. Each workflow node gets pre-warmed pods. |

## Naming

| Question | Context |
|----------|---------|
| Fix spelling | Rename "Fuchsia" → "Fuchsia" across codebase (crate names, directories, references) |
| WorkflowRunner naming | Current name may be confusing — reconsider |
