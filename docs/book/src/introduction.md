# Fuchsia

Fuchsia is an actor-based dataflow runtime. A workflow is a graph of actors;
each actor is a native Rust implementation, a Wasm component, or a Lua script,
all behind a single `Actor` trait. Routing between nodes lives in the graph —
actors don't address each other directly. They receive a message, do work, and
`emit`; the engine delivers each emission to the downstream actors the graph
wires them to.

## What Fuchsia Is

A **library** (not a CLI, not a daemon) that embeds into a host application.
The host:

1. Registers **actor creators** by type name — one creator per actor *kind*
   (`"passthrough"`, `"debounce"`, `"if"`, `"switch"`, `"wasm"`, `"lua"`, …).
2. Provisions a **graph**: adds nodes (each an actor instance with config) and
   edges directly against the engine. An edge leaves from a source node's
   **named output port** (`"out"` by default), so a node can branch.
3. Pushes messages into a node's mailbox. The runtime calls that actor's
   `handle`, the actor emits on a port, and the engine routes the emission to
   that port's successors until the graph goes quiet.

The host owns everything outside the core dataflow loop: where actors come from
(a builtin set, a component store, a config file), what capabilities they're
granted (a state sink, a product's HTTP import, an MQTT client), and how
observability is wired (Fuchsia emits structured `tracing` events; the host
installs the subscriber).

## The handle-per-message model

Actors do **not** run their own receive loop. The runtime owns the loop: it
pulls one message from an actor's mailbox, calls `handle(&ctx, msg)`, records
the outcome, and pulls the next. An actor is three async methods —
`setup` once, `handle` per message, `teardown` on shutdown — over `&mut self`,
so per-actor state is just struct fields, no locking (a single task drives each
actor, so `&mut self` is sound across `.await`). A Rust `handle` can `.await`
I/O without blocking the runtime thread; handling is still sequential, one
`handle` in flight per actor.

The *guest* contract stays synchronous: Wasm components and Lua scripts are
written as straight-line synchronous code (the WIT lifecycle and `emit` are
synchronous from the guest's view). The hosts now drive that synchronous guest
through async wasmtime / mlua, so a guest call can suspend while an async host
import runs. `emit` itself remains a synchronous, non-blocking import — that
keeps the emit path cheap, not that there's no async on the host side.

## Goals

- **Declarative routing.** Actors are decoupled units of work. Their input
  comes from a mailbox; their output is emitted to whatever the graph wires
  downstream. They don't know — and shouldn't know — who consumes it. Topology
  is configuration.
- **Polyglot, one trait.** A graph can mix native Rust actors, Wasm component
  actors (safe third-party plugins), and Lua actors (quick scripting), all
  behind the same `Actor` trait. The engine doesn't distinguish them.
- **Capabilities, not ambient authority.** What an actor can *do* beyond
  receiving and emitting is a typed bag injected at construction —
  [`emit`](./architecture/host-capabilities.md), `schedule` (a self-timer), and
  `state` (a pre-scoped write sink). An actor's struct declares what it holds,
  so it can do exactly that and no more.
- **Host-owns-the-import-set.** Fuchsia owns one contract: `fuchsia:actor`
  (lifecycle + emit + payload types). It does *not* prescribe what else a Wasm
  or Lua actor may call. Products define their own capability surface via the
  `WasmHost` / `LuaHost` seam.
- **Predictable concurrency.** Each actor is its own tokio task with a bounded
  mailbox. Fan-out emerges from graph edges; a full mailbox sheds
  (at-most-once) rather than blocking the producer.

## What Fuchsia Is Not

- **Not classic actors.** Hewitt-style actors address each other by id.
  Fuchsia's actors emit; the *graph* decides where it goes. This is dataflow
  with actor-flavored concurrency, not Erlang or Akka.
- **Not a workflow CLI.** There's no `fuchsia run` binary. Fuchsia is a
  library; the host application is the entry point. (See `crates/fuchsia-examples`
  for a runnable demonstration.)
- **Not a sandboxing tool by itself.** Wasm actors get the isolation wasmtime
  provides; native actors run with the host's full privileges. Fuchsia decides
  *where* code runs and *what capabilities it's handed*; it doesn't define the
  security model.

## Where To Go Next

- [Overview](./architecture/overview.md) — the layered crates and the mental model
- [Runtime & Engine](./architecture/engine.md) — mailboxes, the handle loop, routing, lifecycle
- [Capabilities](./architecture/host-capabilities.md) — the `emit` / `schedule` bag
- [Host Extensibility](./architecture/host-extensibility.md) — defining your own Wasm world or Lua globals
- [Builtins](./runtimes/builtins.md), [WebAssembly actors](./runtimes/wasm.md), [Lua actors](./runtimes/lua.md)
- [Crate map](./reference/crate-map.md) and [Roadmap](./reference/roadmap.md)
