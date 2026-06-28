# Fuchsia [![#FF00FF](https://img.shields.io/badge/%23FF00FF-FF00FF)](https://www.color-hex.com/color/ff00ff)

An actor-based dataflow runtime, in Rust.

A workflow is a graph of actors. Each actor is a native Rust implementation, a
Wasm component, or a Lua script, all behind a single `Actor` trait. Routing
between nodes lives in the graph definition — not in actor code — so topology is
configuration anyone can edit, while actor implementations stay focused on their
own logic.

Actors don't run their own loop. The runtime owns the receive loop: it pulls one
message from an actor's mailbox, calls `handle`, and routes whatever the actor
`emit`s to the downstream actors the graph wires it to. The contract (lifecycle
+ emit) is synchronous, so even Wasm and Lua guests are driven with plain
synchronous calls — no async bridge.

## Highlights

- **Single Actor trait.** Native Rust, Wasm components, and Lua scripts all
  implement `fuchsia_actor::Actor` (`setup` / `handle` / `teardown`, sync, over
  `&mut self`). Graphs mix and match freely.
- **Handle-per-message.** The runtime drives the loop and reports each outcome to
  the message's ack; per-actor state is just struct fields, no locking.
- **Declarative routing.** Actors emit; the engine delivers to successors via a
  live routing table, so graphs can be added or torn down without
  re-instantiating the actors they share.
- **Capabilities, injected.** What an actor can do beyond receive-and-emit is a
  typed bag handed in at construction — `emit` (engine), `schedule` (a self-timer,
  runtime), `state` (a pre-scoped write sink, host). An actor's struct *is* the
  statement of its authority.
- **Host owns the import set.** Fuchsia owns one contract, `fuchsia:actor`
  (lifecycle + emit + payload). What else a Wasm/Lua actor may import is the
  product's, defined through the `WasmHost` / `LuaHost` seam.
- **Per-node concurrency.** Each actor is its own tokio task with a bounded
  mailbox. Fan-out comes from edges; a full mailbox sheds (at-most-once) instead
  of stalling the producer.
- **Tracing facade.** Spans per actor, each handle span parented by the upstream's
  so a trace follows a message across mailbox hops. No subscriber bundled — the
  host installs one.

## Quick Start

Fuchsia is a library, not a CLI. Add the crates you need:

```toml
[dependencies]
fuchsia-actor          = { git = "..." }  # the Actor trait + capability bag + message types
fuchsia-engine         = { git = "..." }  # routing: add_node / add_edge / push
fuchsia-actor-builtins = { git = "..." }  # passthrough, debounce, deadband, dedup, commit
fuchsia-actor-wasm     = { git = "..." }  # host Wasm component actors
fuchsia-actor-lua      = { git = "..." }  # host Lua script actors
```

A minimal graph — push a reading through one builtin to a Wasm guest:

```rust
use std::collections::BTreeMap;
use fuchsia_actor::{ActorCapabilities, ActorConfig, ActorId, Message, COMPONENT_ENV_KEY};
use fuchsia_actor_builtins::DedupCreator;
use fuchsia_actor_wasm::{BaseHost, WasmActorCreator};
use fuchsia_engine::Engine;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::new();
    engine.register("dedup", DedupCreator).await;
    engine.register(
        "wasm",
        WasmActorCreator::new(BaseHost::new())?.with_path("echo", "components/echo.wasm")?,
    ).await;

    let dedup = ActorId::scoped("demo", "dedup");
    let echo  = ActorId::scoped("demo", "echo");

    let mut env = BTreeMap::new();
    env.insert(COMPONENT_ENV_KEY.to_owned(), "echo".to_owned());
    let echo_cfg = ActorConfig { env, settings: Default::default() };

    engine.add_node(dedup.clone(), "dedup", &ActorConfig::default(), ActorCapabilities::new()).await?;
    engine.add_node(echo.clone(),  "wasm",  &echo_cfg,               ActorCapabilities::new()).await?;
    engine.add_edge(dedup.clone(), echo.clone())?;

    engine.push(&dedup, Message::json("reading", json!(42)))?;
    Ok(())
}
```

For a complete runnable demo wiring a Lua actor, a builtin, and a Wasm component
into one graph, see [`crates/fuchsia-examples`](./crates/fuchsia-examples):

```bash
(cd test-components/actor-echo && cargo component build --release)
cargo run -p fuchsia-examples
```

See the [mdBook documentation](./docs/book/src/) for the full architecture and
the per-implementation guides.

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo fmt
```

The Wasm integration test (and the example) need the test component built first;
they skip / print instructions otherwise:

```bash
cd test-components/actor-echo && cargo component build --release
```

Benches use criterion — see [`.claude/skills/bench/SKILL.md`](./.claude/skills/bench/SKILL.md):

```bash
cargo bench -p fuchsia-runtime --bench runtime
```

## Layout at a Glance

- `crates/fuchsia-actor` — the contract: `Actor` trait, capability bag, `Message`, creator/registry
- `crates/fuchsia-transport` — bounded mailbox + delivery/ack plumbing
- `crates/fuchsia-runtime` — the handle-per-message loop; provides `schedule`
- `crates/fuchsia-engine` — routing between actors per graph edges; provides `emit`
- `crates/fuchsia-actor-builtins` — native builtin actors
- `crates/fuchsia-actor-wasm` — Wasm-component-hosting actors
- `crates/fuchsia-actor-lua` — Lua-script-hosting actors
- `crates/fuchsia-examples` — runnable mixed-runtime demo

See [`docs/book/src/reference/crate-map.md`](./docs/book/src/reference/crate-map.md)
for dependencies and a fuller map.
