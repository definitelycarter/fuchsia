# Fuchsia

Fuchsia is an actor-based dataflow runtime. Workflows are graphs of actors
connected by tokio mpsc channels; each actor is either a native Rust
implementation, a Wasm component, or a Lua script, all behind a single
`Actor` trait. The graph is declared in JSON, and routing between nodes
lives in the graph itself — actors don't address each other directly.

## What Fuchsia Is

A **library** (not a CLI, not a daemon) that embeds into a host application.
The host:

1. Builds a registry of actors it wants to make available.
2. Loads or composes a `Graph` describing how those actors are wired
   together.
3. Hands the graph to an `Orchestrator`, which spawns one tokio task per
   node, wires bounded mpsc channels per edge, and stands back.
4. Pushes messages into the graph's entry point. The runtime delivers
   results to downstream actors through the channels until the topology
   drains.

The host owns everything outside the core dataflow loop: where actors come
from (a plugin store, a hard-coded list, a config file), what capabilities
they have access to (HTTP clients, KV stores, MQTT brokers — whatever the
host implements), and how observability is wired (Fuchsia emits structured
`tracing` events; the host installs the subscriber).

## Goals

- **Actor model with declarative routing.** Actors are decoupled units of
  work. Their inputs come from the channel; their output is emitted to
  whatever downstream the graph wires them to. They don't know — and
  shouldn't know — who consumes their output.
- **Polyglot, one trait.** A workflow can mix native Rust actors, Wasm
  component actors (for safe third-party plugins), and Lua actors (for
  quick scripting), all behind the same `Actor` trait. The orchestrator
  doesn't distinguish them.
- **Host-extensible capabilities.** Universal capabilities — HTTP, log —
  ship with Fuchsia. Anything beyond that (MQTT, BLE, Modbus, GPU-compute,
  custom databases) is the host's domain. Hosts define their own WIT world
  for Wasm actors or register their own Lua globals; the runtime stays
  out of it.
- **Predictable concurrency.** Each node is its own tokio task. Fan-out
  emerges from graph edges. Backpressure comes for free from bounded
  channels. Cancellation cascades from one `CancellationToken`.

## What Fuchsia Is Not

- **Not classic actors.** Hewitt-style actors send messages to other
  actors by address. Fuchsia's actors return values; the *graph* decides
  where they go. This is dataflow with actor-flavored concurrency, not
  Erlang or Akka.
- **Not a workflow CLI.** There's no `fuchsia run` binary. Fuchsia is a
  library; the host application is the entry point.
- **Not a sandboxing tool by itself.** Wasm actors get the isolation
  wasmtime provides; native Rust actors run with the host's full
  privileges. Fuchsia decides *where* code runs (which actor implementation
  hosts it); it doesn't define the security model.

## Where To Go Next

- [Overview](./architecture/overview.md) — how the pieces fit together
- [Runtime](./architecture/engine.md) — Graph, Registry, Orchestrator,
  channels, cancellation
- [Capabilities](./architecture/host-capabilities.md) — HTTP, log routing,
  capability injection
- [Host Extensibility](./architecture/host-extensibility.md) — defining
  your own WIT world or Lua globals
- [WebAssembly actors](./runtimes/wasm.md) and [Lua actors](./runtimes/lua.md)
- [Crate map](./reference/crate-map.md) and [Roadmap](./reference/roadmap.md)
