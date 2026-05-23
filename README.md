# Fuchsia [![#FF00FF](https://img.shields.io/badge/%23FF00FF-FF00FF)](https://www.color-hex.com/color/ff00ff)

An actor-based dataflow runtime, in Rust.

Workflows are graphs of actors connected by tokio mpsc channels. Each actor
is either a native Rust implementation, a Wasm component, or a Lua script,
all behind a single `Actor` trait. Routing between nodes lives in the graph
definition — not in actor code — so workflow topology is a piece of
configuration that anyone can edit, while actor implementations stay
focused on their own logic.

Hosts compose a runtime by registering actors into an `ActorRegistry` and
starting graphs against an `Orchestrator`. Universal capabilities (HTTP,
log) ship with Fuchsia; domain-specific capabilities (MQTT, BLE, a custom
database client, anything) are defined by the host and registered into its
own `WasmHost` / `LuaHost` implementation.

## Highlights

- **Single Actor trait.** Native Rust, Wasm components, and Lua scripts all
  implement `fuchsia_actor::Actor`. Workflows mix and match freely.
- **Dataflow, not classic actors.** Each actor produces output and the
  runtime delivers it to downstream nodes per the graph's edges. Actors
  don't address each other directly — topology is declared in JSON.
- **Per-node concurrency.** Every node is its own tokio task. Independent
  branches run in parallel automatically. Backpressure and fan-out are
  inherited from bounded mpsc channels.
- **Host-extensible.** `WasmHost` and `LuaHost` traits let the embedder
  define its own WIT world (for Wasm) or Lua globals (for scripts), so
  domain-specific capabilities slot in without forking the runtime.
- **Tracing facade.** Spans on workflow lifecycle, spans per actor,
  trace-level events on every message. No subscriber is bundled — the
  consuming application installs `tracing-subscriber`,
  `tracing-opentelemetry`, or whatever it prefers.
- **Cancellation cascades.** A single `CancellationToken` reaches every
  actor. When the entry channel closes or `WorkflowHandle::cancel()` is
  called, the whole topology drains cleanly.

## Quick Start

Fuchsia is a library, not a CLI. Add the crates you need to your project:

```toml
[dependencies]
fuchsia-actor       = { git = "..." }   # the Actor trait + I/O primitives
fuchsia-runtime     = { git = "..." }   # registry + graph + orchestrator
fuchsia-capabilities = { git = "..." }  # HTTP capability (optional)
fuchsia-actor-wasm  = { git = "..." }   # if you want to host Wasm actors
fuchsia-actor-lua   = { git = "..." }   # if you want to host Lua actors
```

A minimal program:

```rust
use std::sync::Arc;
use fuchsia_runtime::{ActorRegistry, Graph, Orchestrator};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut registry = ActorRegistry::new();
    // registry.register::<MyActor, MyConfig, _>("my.actor", |cfg| MyActor::new(cfg));

    let graph = serde_json::from_str::<Graph>(/* your graph JSON */)?;
    let orchestrator = Orchestrator::new(Arc::new(registry));
    let handle = orchestrator.start(&graph)?;

    handle.send(json!({ "event": "hello" })).await?;
    let _results = handle.join().await;
    Ok(())
}
```

See the [mdBook documentation](./docs/book/src/) for the full architecture
and the per-actor-implementation guides.

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo fmt
```

The Wasm integration test requires the test component to be built first:

```bash
cd test-components/test-actor-component && cargo component build --release
```

Benches use criterion — see [`.claude/skills/bench/SKILL.md`](./.claude/skills/bench/SKILL.md):

```bash
cargo bench -p fuchsia-runtime --bench chain_throughput
cargo bench -p fuchsia-runtime --bench fan_out
```

## Layout at a Glance

- `crates/fuchsia-actor` — the `Actor` trait + `Inbox` / `Emitter` / `Context` / `ActorError`
- `crates/fuchsia-runtime` — the engine (Graph, Registry, Orchestrator)
- `crates/fuchsia-capabilities` — universal capabilities (HTTP)
- `crates/fuchsia-actor-wasm` — Wasm-component-hosting Actor implementation
- `crates/fuchsia-actor-lua` — Lua-script-hosting Actor implementation

See [`docs/book/src/reference/crate-map.md`](./docs/book/src/reference/crate-map.md)
for dependencies and a more detailed map.
