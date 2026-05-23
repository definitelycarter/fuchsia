# Fuschia Codebase Analysis

Bottom-to-top review of the codebase. Findings organized by severity, with specific file paths and line references.

> **Last updated** after the runtime unification refactor: `fuschia-runtime`, `fuschia-engine`, `fuschia-host`, `fuschia-task-host`, `fuschia-task` have been removed. Workflows are now invoked manually with a payload that flows into entry-point nodes.

## Bugs (broken right now)

### 1. Epoch interruption is a no-op

> **Status: Partially resolved** — The original files (`fuschia-runtime`) are deleted. The `WasmExecutor` in `fuschia-task-runtime-wasm` may still need to call `engine.increment_epoch()` for timeouts to work.

### ~~2. `config::get()` WIT import always returns None~~

> **Status: Resolved** — `fuschia-host` and `fuschia-task-host` have been deleted. Config is now provided through the `Capabilities` struct and wired per-node by the orchestrator.

### 3. Lexicographic version sort

**File:** `crates/fuschia-component-registry/src/fs_registry.rs`

Registry uses plain string comparison for "latest" version. `10.0.0` sorts lower than `9.0.0`. Should use the `semver` crate.

## High-risk fragilities

### ~~4. `futures::executor::block_on` in async context~~

> **Status: Resolved** — `fuschia-task-host` has been deleted. The Lua runtime uses `block_on` for host capabilities but this is documented as an open question in the Lua runtime docs.

### ~~5. Missing explicit `wasm_component_model(true)`~~

> **Status: Resolved** — `fuschia-runtime` has been deleted. The `WasmExecutor` in `fuschia-task-runtime-wasm` owns its own engine configuration.

### 6. `dirs::home_dir().expect()` can panic

**File:** `src/main.rs`

Panics in environments where the home directory cannot be determined (Docker containers with no HOME set, certain CI environments). Should return an error instead.

### ~~7. TOCTOU race in component cache~~

> **Status: Resolved** — `fuschia-runtime/src/cache.rs` has been deleted. The `WasmExecutor` in `fuschia-task-runtime-wasm` owns its own caching strategy.

## Dead code

### ~~8. Orphan crates~~

> **Status: Resolved** — Orphan crates (`fuschia-task`, `fuschia-engine`) have been removed from the workspace.

### ~~9. Dead code in fuschia-host~~

> **Status: Resolved** — `fuschia-host` crate has been deleted.

### 10. Dead `WorkflowError` enum

**File:** `crates/fuschia-workflow/src/error.rs`

Defines 5 variants (`NodeNotFound`, `InvalidEdge`, `NoEntryPoints`, `ComponentResolutionFailed`, `DigestMismatch`). None are ever constructed or returned anywhere. The resolver uses `ResolveError` and the orchestrator uses `OrchestratorError`.

### ~~11. Dead `load_component` exports~~

> **Status: Resolved** — `fuschia-task-host` has been deleted.

### 12. Dead `RegistryError::NotFound` variant

**File:** `crates/fuschia-component-registry/src/error.rs`

Variant exists but is never constructed. Only `VersionNotFound` is used.

## Design issues

### 13. Graph rebuilt on every call

**File:** `crates/fuschia-workflow/src/workflow.rs`

`workflow.graph()` constructs a new `Graph` with fresh HashMaps every time. Called multiple times per execution loop iteration. Should be built once and cached.

### 15. Workflow-level `timeout_ms` and `max_retry_attempts` are dead fields

**File:** `crates/fuschia-workflow/src/workflow.rs`

Carried through the pipeline but never read by the orchestrator. Only per-node `timeout_ms` is forwarded to executors. Users who set workflow-level values get no effect and no error.

### 16. `fail_workflow` field carried but never checked

**File:** `crates/fuschia-workflow/src/node.rs`

Resolver sets it, orchestrator ignores it. No code ever reads this field.

### ~~17. `RuntimeError::InvalidGraph` is a catch-all~~

> **Status: Resolved** — `fuschia-runtime` has been deleted. The orchestrator uses `OrchestratorError` with distinct variants: `Validation`, `TaskExecution`, `InputResolution`, `ComponentNotFound`, `ComponentIO`, `InternalError`.

### 18. Resolver silently drops retry config

**File:** `crates/fuschia-resolver/src/resolver.rs`

`retry_backoff` and `retry_initial_delay_ms` from `WorkflowDef` are not passed through during resolution. Users who configure these fields get no error and no effect.

### 20. Non-deterministic entry_points order

**File:** `crates/fuschia-workflow/src/graph.rs`

`entry_points` is built by iterating `HashMap::keys()`, which has no guaranteed order. Could affect execution order in subtle ways.

### 21. `_payload` magic string in invoke_node

**File:** `crates/fuschia-workflow-orchestrator/src/orchestrator.rs`

Payload is inserted as `upstream_data.insert("_payload".to_string(), ...)`. Undocumented magic key that template authors must know about. Collision risk if a node is named `_payload`.

## Code quality

### ~~22. Massive duplication between host crates~~

> **Status: Resolved** — `fuschia-task-host` has been deleted. Host capabilities are now shared through the `Capabilities` struct, with each runtime writing thin glue to wire its VM's calling convention.

### ~~23. Linker recreated per execution~~

> **Status: Resolved** — `fuschia-task-host` has been deleted. The `WasmExecutor` in `fuschia-task-runtime-wasm` owns its own linker strategy.

### 24. `unwrap()` in production code

**File:** `crates/fuschia-workflow-orchestrator/src/orchestrator.rs`

Review orchestrator for unwrap calls that should use `.expect()` or proper error handling.

### 25. No tracing subscriber initialized

**File:** `src/main.rs`

Every crate uses `tracing::info!`/`tracing::error!` but main.rs never initializes a `tracing_subscriber`. All tracing output is silently dropped at runtime.

### 26. ~90% duplication between `run_workflow` and `run_node`

**File:** `src/main.rs`

Both functions share identical setup (tokio runtime, read workflow, parse JSON, read stdin, create registry, resolver, resolve, create Orchestrator). Only difference is `invoke()` vs `invoke_node()`. Should be factored into a shared setup function.

### 27. `completed.clone()` copies entire results map

**File:** `crates/fuschia-workflow-orchestrator/src/orchestrator.rs`

`run_execution_loop` returns `InvokeResult { node_results: completed.clone() }`. Each `NodeResult` contains `serde_json::Value` outputs, so this clones all workflow results. `std::mem::take` would avoid this.

### 28. No serde round-trip tests in fuschia-config

**File:** `crates/fuschia-config/`

Complex `#[serde(flatten)]` nesting across three levels with zero test coverage. Deserialization could break silently.

### 29. YAML documented but not supported

**File:** `src/main.rs`

CLI help says "Path to the workflow file (JSON or YAML)" but only `serde_json::from_str` is used. YAML is not supported.

### ~~30. Hardcoded `InMemoryKvStore`~~

> **Status: Resolved** — `fuschia-task-host` has been deleted. KV is now provided through the `Capabilities` struct, allowing any `KvStore` implementation to be injected per-node.

### 31. Sequential node resolution in resolver

**File:** `crates/fuschia-resolver/src/resolver.rs`

Each node is resolved one at a time with a `for` loop over async registry lookups. Could be parallelized with `futures::join_all`.

### 32. Unused `serde_json` dependency in fuschia-config

**File:** `crates/fuschia-config/Cargo.toml`

`serde_json = "1"` is declared as a dependency but never used in any source file in the crate.

### 33. `parse_dir_name` ambiguous with multi-segment names

**File:** `crates/fuschia-component-registry/src/fs_registry.rs`

Uses `rfind("--")` for version and `find("--")` for org separator. A component `my-org--sub-team--tool` at version `1.0.0` stored as `my-org--sub-team--tool--1.0.0` would parse incorrectly.

### 34. No digest verification on install

**File:** `crates/fuschia-component-registry/src/fs_registry.rs`

`install()` copies files but does not compute or verify the wasm binary's SHA-256 digest against the manifest's `digest` field.
