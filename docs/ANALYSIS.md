# Fuchsia Codebase Analysis

Bottom-to-top review of the codebase. Findings organized by severity, with specific file paths and line references.

## Bugs (broken right now)

### 1. Epoch interruption is a no-op

**Files:** `crates/fuchsia-runtime/src/runtime.rs:57-58`, `crates/fuchsia-runtime/src/task.rs:104`, `crates/fuchsia-runtime/src/trigger.rs`

The engine enables `epoch_interruption(true)` and executors call `set_epoch_deadline`, but `engine.increment_epoch()` is never called anywhere in the codebase. Component timeouts are completely non-functional. A wasm component that loops forever will never be interrupted.

### 2. `config::get()` WIT import always returns None

**Files:** `crates/fuchsia-host/src/state.rs:39`, `crates/fuchsia-task-host/src/bindings.rs:62-64`, `crates/fuchsia-trigger-host/src/bindings.rs`

The `set_config()` method exists on `HostState` but is never called anywhere. The config HashMap is always empty, so the `config::get()` WIT host import always returns `None` for every component.

### 3. Lexicographic version sort

**File:** `crates/fuchsia-component-registry/src/fs_registry.rs:119`

Registry uses plain string comparison for "latest" version: `b.manifest.version.cmp(&a.manifest.version)`. This means `10.0.0` sorts lower than `9.0.0`, and `1.10.0` sorts lower than `1.9.0`. Should use the `semver` crate.

## High-risk fragilities

### 4. `futures::executor::block_on` in async context

**Files:** `crates/fuchsia-task-host/src/bindings.rs:50-58`, `crates/fuchsia-trigger-host/src/bindings.rs`

Both task-host and trigger-host use `futures::executor::block_on()` to bridge sync WIT host imports to the async `KvStore` trait. Works only because `InMemoryKvStore` resolves immediately. Any real async backend (Redis, etc.) will deadlock the tokio runtime.

### 5. Missing explicit `wasm_component_model(true)`

**File:** `crates/fuchsia-runtime/src/runtime.rs:56-58`

The runtime's engine config enables `async_support` and `epoch_interruption` but does not call `wasm_component_model(true)`. Only works because the wasmtime `component-model` cargo feature enables it by default. The dead `create_engine` in fuchsia-host does call it explicitly, highlighting the inconsistency.

### 6. `dirs::home_dir().expect()` can panic

**File:** `src/main.rs:60`

Panics in environments where the home directory cannot be determined (Docker containers with no HOME set, certain CI environments). Should return an error instead.

### 7. TOCTOU race in component cache

**File:** `crates/fuchsia-runtime/src/cache.rs:50-72`

Read lock is acquired, checked, and released. Then a write lock is acquired to insert. Between the two lock acquisitions, another thread could compile and insert the same component. Harmless (double compilation, second insert overwrites) but wastes work.

## Dead code

### 8. Orphan crates

Five crates are in the workspace `members` list but are not depended on by any other crate or the binary:

- **fuchsia-task** — Contains a hardcoded error stub. Superseded by fuchsia-task-host.
- **fuchsia-trigger** — Defines `TriggerError` which is never returned. Superseded by fuchsia-trigger-host.
- **fuchsia-artifact** — `fs.rs` accepts `_content_type` but never stores it. Not depended on.
- **fuchsia-world** — Generates bindings for a combined `host` world nobody imports. Task-host and trigger-host generate their own.
- **fuchsia-engine** — Not depended on by the binary. CLI uses `fuchsia-runtime` directly.

### 9. Dead code in fuchsia-host

**File:** `crates/fuchsia-host/src/`

- `create_engine` and `EngineConfig` (`engine.rs:6-35`) — never used by any crate
- `create_store` (`state.rs:56-67`) — exported but never called
- `HostState.table` ResourceTable (`state.rs:13`) — initialized but never read
- `set_config()` (`state.rs:39`) — never called (see bug #2)

### 10. Dead `WorkflowError` enum

**File:** `crates/fuchsia-workflow/src/error.rs`

Defines 5 variants (`NodeNotFound`, `InvalidEdge`, `NoEntryPoints`, `ComponentResolutionFailed`, `DigestMismatch`). None are ever constructed or returned anywhere. The resolver uses `ResolveError` and the runtime uses `RuntimeError`.

### 11. Dead `load_component` exports

**Files:** `crates/fuchsia-task-host/src/executor.rs:63-66`, `crates/fuchsia-trigger-host/src/executor.rs:64-67`

Exported as public but never called. The runtime has its own `load_component`/`load_trigger_component`.

### 12. Dead `RegistryError::NotFound` variant

**File:** `crates/fuchsia-component-registry/src/error.rs:7`

Variant exists but is never constructed. Only `VersionNotFound` is used.

## Design issues

### 13. Graph rebuilt on every call

**File:** `crates/fuchsia-workflow/src/workflow.rs:22`

`workflow.graph()` constructs a new `Graph` with fresh HashMaps every time. Called twice per execution loop iteration (`find_ready_nodes` at line 427 and `execute_ready_nodes` at line 445 of runtime.rs). Should be built once and cached.

### 14. `JoinStrategy` is never checked

**File:** `crates/fuchsia-runtime/src/runtime.rs:582-595`

The `execute_join` function unconditionally passes input through as output. There is no distinction between `JoinStrategy::All` and `JoinStrategy::Any`. The strategy field is carried through the entire pipeline and then ignored.

### 15. Workflow-level `timeout_ms` and `max_retry_attempts` are dead fields

**File:** `crates/fuchsia-workflow/src/workflow.rs:14-15`

Carried through the pipeline but never read by the runtime. Only per-node `timeout_ms` is forwarded to executors. Users who set workflow-level values get no effect and no error.

### 16. `fail_workflow` field carried but never checked

**File:** `crates/fuchsia-workflow/src/node.rs:16`

Resolver sets it, runtime ignores it. No code ever reads this field.

### 17. `RuntimeError::InvalidGraph` is a catch-all

**File:** `crates/fuchsia-runtime/src/error.rs`

Used for 7+ unrelated error conditions: engine creation failure, task join errors, component not loaded, trigger in wrong position, loop not implemented, node not found, channel close. Should be distinct variants.

### 18. Resolver silently drops retry config

**File:** `crates/fuchsia-resolver/src/resolver.rs:239`

`retry_backoff` and `retry_initial_delay_ms` from `WorkflowDef` are not passed through during resolution. Users who configure these fields get no error and no effect.

### 19. Near-identical `LockedComponent` and `LockedTriggerComponent`

**File:** `crates/fuchsia-workflow/src/node.rs:52-83`

Share 4 of 5 fields (`name`, `version`, `digest`, `input_schema`). Only difference is `task_name` vs `trigger_name`. Could be unified.

### 20. Non-deterministic entry_points order

**File:** `crates/fuchsia-workflow/src/graph.rs:43-47`

`entry_points` is built by iterating `HashMap::keys()`, which has no guaranteed order. Could affect execution order in subtle ways.

### 21. `_payload` magic string in invoke_node

**File:** `crates/fuchsia-runtime/src/runtime.rs:176`

Payload is inserted as `upstream_data.insert("_payload".to_string(), ...)`. Undocumented magic key that template authors must know about. Collision risk if a node is named `_payload`.

## Code quality

### 22. Massive duplication between task-host and trigger-host

**Files:** `crates/fuchsia-task-host/src/bindings.rs`, `crates/fuchsia-trigger-host/src/bindings.rs`

~90 lines of identical host import implementations (kv, config, log) copy-pasted. Should be a shared implementation.

### 23. Linker recreated per execution

**Files:** `crates/fuchsia-task-host/src/executor.rs:43-49`, `crates/fuchsia-trigger-host/src/executor.rs:43-51`

A new `Linker` is created and populated with all host imports on every `execute_task`/`execute_trigger` call. The Linker only depends on the Engine and host import signatures, both stable. Should be created once and reused.

### 24. `unwrap()` in runtime production code

**File:** `crates/fuchsia-runtime/src/runtime.rs:357-358`

`find_trigger_nodes().into_iter().next().unwrap()` and `workflow.get_node(&trigger_id).unwrap()`. Logically safe (validated above) but violates AGENTS.md guideline. Should use `.expect("validated by validate_workflow")` at minimum.

### 25. No tracing subscriber initialized

**File:** `src/main.rs`

Every crate uses `tracing::info!`/`tracing::error!` but main.rs never initializes a `tracing_subscriber`. All tracing output is silently dropped at runtime.

### 26. ~90% duplication between `run_workflow` and `run_node`

**File:** `src/main.rs:79-162`

Both functions share identical setup (tokio runtime, read workflow, parse JSON, read stdin, create registry, resolver, resolve, create RuntimeConfig and Runtime). Only difference is `invoke()` vs `invoke_node()`. Should be factored into a shared setup function.

### 27. `completed.clone()` copies entire results map

**File:** `crates/fuchsia-runtime/src/runtime.rs:268`

`run_execution_loop` returns `InvokeResult { node_results: completed.clone() }`. Each `NodeResult` contains `serde_json::Value` outputs, so this clones all workflow results. `std::mem::take` would avoid this.

### 28. No serde round-trip tests in fuchsia-config

**File:** `crates/fuchsia-config/`

Complex `#[serde(flatten)]` nesting across three levels with zero test coverage. Deserialization could break silently.

### 29. YAML documented but not supported

**File:** `src/main.rs:45`

CLI help says "Path to the workflow file (JSON or YAML)" but only `serde_json::from_str` is used. YAML is not supported.

### 30. Hardcoded `InMemoryKvStore`

**File:** `crates/fuchsia-task-host/src/bindings.rs:13`

`TaskHostState` hardcodes `HostState<InMemoryKvStore>`. The generic type parameter on `HostState<K: KvStore>` is wasted — there is no way to inject a different KV backend.

### 31. Sequential node resolution in resolver

**File:** `crates/fuchsia-resolver/src/resolver.rs:233-237`

Each node is resolved one at a time with a `for` loop over async registry lookups. Could be parallelized with `futures::join_all`.

### 32. Unused `serde_json` dependency in fuchsia-config

**File:** `crates/fuchsia-config/Cargo.toml`

`serde_json = "1"` is declared as a dependency but never used in any source file in the crate.

### 33. `parse_dir_name` ambiguous with multi-segment names

**File:** `crates/fuchsia-component-registry/src/fs_registry.rs:37-52`

Uses `rfind("--")` for version and `find("--")` for org separator. A component `my-org--sub-team--tool` at version `1.0.0` stored as `my-org--sub-team--tool--1.0.0` would parse incorrectly.

### 34. No digest verification on install

**File:** `crates/fuchsia-component-registry/src/fs_registry.rs`

`install()` copies files but does not compute or verify the wasm binary's SHA-256 digest against the manifest's `digest` field.
