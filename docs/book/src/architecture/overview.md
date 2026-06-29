# Overview

Fuchsia is a stack of small crates, each depending only on the layer below it.
From the bottom up:

```text
┌──────────────────────────────────────────────────────────────────────┐
│ Host application                                                       │
│   registers actor creators · provisions graphs · pushes messages      │
└───────────────────────────────┬────────────────────────────────────────┘
                                │ add_node / add_edge / push
                                ▼
┌──────────────────────────────────────────────────────────────────────┐
│ fuchsia-engine — routing                                              │
│   Engine: add_node / add_edge / push; routes each emit to the         │
│   mailboxes wired to its output port per the graph's edges (`emit`)   │
└───────────────────────────────┬────────────────────────────────────────┘
                                │ spawns / delivers to
                                ▼
┌──────────────────────────────────────────────────────────────────────┐
│ fuchsia-runtime — the actor substrate                                 │
│   owns the recv→handle loop, one tokio task per actor; provides       │
│   `schedule`. fuchsia-transport supplies the bounded mailbox + ack.   │
└───────────────────────────────┬────────────────────────────────────────┘
                                │ drives `Actor`
                                ▼
┌──────────────────────────────────────────────────────────────────────┐
│ Actors — everything below implements fuchsia_actor::Actor             │
│  ┌─────────────┐  ┌──────────────────┐  ┌──────────────────┐          │
│  │ builtins    │  │ fuchsia-actor-wasm│  │ fuchsia-actor-lua │          │
│  │ (native)    │  │  WasmActor<H>    │  │  LuaActor<H>     │          │
│  └─────────────┘  └────────┬─────────┘  └────────┬─────────┘          │
│                            │ WasmHost            │ LuaHost            │
│                            ▼                     ▼                     │
│                   product-defined capability imports (or BaseHost)    │
└──────────────────────────────────────────────────────────────────────┘
```

`fuchsia-actor` (not drawn) sits beneath everything — it defines the `Actor`
trait, the capability bag, the message type, and the creator/registry, and is
the only crate every other one depends on.

## The mental model

An **actor** implements [`fuchsia_actor::Actor`](https://docs.rs/fuchsia-actor):
three async methods over `&mut self` — `setup(ctx)` once, `handle(ctx,
msg)` per message, `teardown(ctx)` on shutdown. The actor receives a `Message`,
does work, and `emit`s; it does *not* know who receives its output.

The runtime owns the loop. There is no `run(inbox, …)` an actor drives itself;
the runtime pulls one message from the actor's **mailbox**, calls `handle`, and
reports the outcome to the message's **ack**. This is the *handle-per-message*
model — per-actor state lives in struct fields, never behind a lock, because
only the runtime's single task touches a given actor.

An **actor creator** (`ActorCreator::create(config, caps) -> Box<dyn Actor>`)
builds an actor from its per-instance `ActorConfig` and the `ActorCapabilities`
granted to it. Creators are registered by **type name**; one creator backs a
whole kind of node (every `"debounce"` node, every `"wasm"` node).

The **engine** turns nodes and edges into a running graph. `add_node`
instantiates an actor (through its creator) and registers its mailbox as a
routable target; `add_edge(from, port, to)` records that one node's emissions
*on a named output port* flow to another's mailbox. When an actor emits on a
port, the engine looks up the successors wired to that port in a live routing
table and delivers to each. `push` injects an external event into
one entrypoint's mailbox. The graph is configuration the host builds directly;
how that configuration is authored or persisted is a product concern, not the
runtime's.

## Why dataflow, not classic actors

Classic actor models (Hewitt, Erlang, Akka) have actors address each other:
`ctx.send(other_pid, msg)`. The topology is encoded in actor code.

Fuchsia takes the opposite stance: topology is configuration. Actors emit; the
graph wires them. So:

- Topology is edited as graph configuration, not Rust/Wasm/Lua.
- Actors stay decoupled from any particular use case.
- Routing changes don't require rebuilding actors — the routing table is
  mutable, so a graph can be added or torn down without re-instantiating the
  actors it shares.

You give up dynamic routing from inside an actor and gain a clean split between
"what this code does" and "where its output goes."

## Three actor flavors, one trait

The same `Actor` trait covers:

1. **Native Rust actors** — implement the trait directly. Best for
   performance-critical or trusted code and protocol adapters. The conditioning
   operators in [`fuchsia-actor-builtins`](../runtimes/builtins.md) are native.
2. **Wasm component actors** — `WasmActor<H: WasmHost>` from
   [`fuchsia-actor-wasm`](../runtimes/wasm.md). Best for safe, sandboxed,
   third-party extension.
3. **Lua script actors** — `LuaActor<H: LuaHost>` from
   [`fuchsia-actor-lua`](../runtimes/lua.md). Best for quick scripting and
   config-driven transforms.

The engine doesn't distinguish them — all three are `Actor`s behind a creator
registered under a type name.

## Capabilities are injected, not ambient

An actor's powers beyond receive-and-emit come from `ActorCapabilities`, a typed
bag handed to its creator at construction. The engine contributes `emit`
(routing through this engine); the runtime contributes `schedule` (a self-timer);
the host contributes scoped I/O like a `state` write sink. An actor
pulls only what it uses and stores it as a field, so its struct *is* the
statement of what it can do. See [Capabilities](./host-capabilities.md).

## Host responsibilities

Fuchsia is deliberately minimal. The host owns:

- **Where actors come from** — a builtin set, a component catalog, a manifest.
- **What capabilities exist beyond the core three** — Fuchsia ships `emit`,
  `schedule`, and `state`. HTTP, KV, MQTT, BLE, and the like are product
  capabilities, wired through the host's own `WasmHost` / `LuaHost`.
- **Integrity, versioning, install** — when loading a Wasm component the host
  verifies digests, picks the version, manages allow-lists. Fuchsia executes
  what it's handed.
- **Observability** — Fuchsia emits `tracing` events (and parents each handle
  span by the upstream's, so traces follow a message across mailbox hops); the
  host installs the subscriber.

## Next

- [Runtime & Engine](./engine.md) — mailboxes, the handle loop, routing, lifecycle
- [Capabilities](./host-capabilities.md) — the `emit` / `schedule` bag
- [Host Extensibility](./host-extensibility.md) — adding your own capability imports
