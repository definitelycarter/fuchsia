# Roadmap

Outstanding features, known gaps, and open questions.

## Features

| Feature | Description | Notes |
|---------|-------------|-------|
| HTTP outbound for components | Add `wasi:http/outgoing-handler` to platform world | Requires wasmtime-wasi-http integration |
| Join node handling | Wait for branches, apply strategy (All/Any) | `fuchsia-workflow-orchestrator` |
| Loop execution | Iterate over collection, execute nested workflow | `fuchsia-workflow-orchestrator` |
| Retry logic | Retry failed nodes per policy | `fuchsia-workflow-orchestrator` |
| Observability | OpenTelemetry tracing to Jaeger | Across crates |
| Component packaging | Bundle manifest + wasm + readme + assets into .fcpkg | Needs CLI tooling |
| CLI improvements | Dedicated `fuchsia-cli` crate | Medium priority |

## Gaps

### fuchsia-config

| Gap | Priority |
|-----|----------|
| Missing retry fields on `NodeDef` — `retry_backoff` and `retry_initial_delay_ms` only on `WorkflowDef`, not `NodeDef` | Medium |

### fuchsia-workflow

| Gap | Priority |
|-----|----------|
| `Graph::downstream()`/`upstream()` return `&[]` for missing nodes — can't distinguish "no edges" vs "node doesn't exist" | Medium |

### fuchsia-resolver

| Gap | Priority |
|-----|----------|
| Join node validation — doesn't validate that `Join` nodes actually have multiple incoming edges | Medium |

### fuchsia-workflow-orchestrator

| Gap | Priority |
|-----|----------|
| Required field validation — no validation that required input fields are present | Medium |
| Digest verification — verify component wasm SHA-256 against `LockedComponent.digest` at load time | High |

### fuchsia-component-registry

| Gap | Priority |
|-----|----------|
| Digest verification — digest in manifest never verified against actual wasm binary on install | Medium |

## Open Questions

| Question | Context |
|----------|---------|
| HTTP outbound filtering | How to enforce `allowed_hosts` with `wasmtime-wasi-http`? Custom `WasiHttpView` wrapper or implement own handler? |
| Path expression parsing location | Should live in `fuchsia-config` (parse at config time) or `fuchsia-workflow-orchestrator` (parse at execution)? |
| Graph method return types | Should `downstream()`/`upstream()` return `Option<&[String]>` instead of `&[]`? |
| Loop item injection | How does `{ "item": {...}, "index": 0 }` get passed to nested workflow inputs? |
| Join node output shape | What's the output — aggregated map of branch outputs? Pass-through? |
| KV store value types | Should kv.wit support complex types (json, number, bool, object) or just strings? |
| Distributed execution model | Daemon mode for production: init container pulls components at deploy time, pods wait for messages from broker. Each workflow node gets pre-warmed pods. Message format: `{execution_id, task_id, input}`. Orchestrator resolves templates, workers just execute. |

## Housekeeping

| Item | Priority |
|------|----------|
| Fix spelling: rename "fuchsia" to "fuchsia" across codebase (crate names, directories, references) | Low |
