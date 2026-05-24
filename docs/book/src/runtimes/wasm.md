# WebAssembly Actors

`fuchsia-actor-wasm` hosts WebAssembly components as Fuchsia actors. Each
`WasmActor` wraps a compiled wasm component. When the actor starts, the
runtime builds one `wasmtime::Store`, instantiates the component into it,
and drives the component's lifecycle: `setup` once, `handle` per inbound
message, `teardown` once on shutdown. The store persists for the actor's
lifetime — connections, subscriptions, and capability handles opened in
`setup` stay live across every `handle` call.

## Anatomy

```rust
pub struct WasmActor<H: WasmHost> {
    engine: Engine,                    // shared, cheap to clone (Arc)
    component: Component,              // compiled, cheap to clone (Arc)
    linker: Arc<Linker<H::State>>,     // built once at build() time
    host: Arc<H>,                      // shared
    epoch_deadline: u64,
}
```

`WasmActor: Clone` is cheap — every field is either `Arc`-backed
internally or wrapped in `Arc`. Each clone produces an independent actor
that builds its own store when started, so the `ActorRegistry` factory
closure can hand back fresh actors per registered node.

## Builder

```rust
use fuchsia_actor_wasm::{WasmActor, DefaultHost};
use fuchsia_capabilities::http::{ReqwestHttp, AllowedHosts};

let engine = build_wasmtime_engine();  // shared across all actors
let http = Arc::new(ReqwestHttp::new(AllowedHosts::new(["api.example.com"])));
let host = DefaultHost::new(http);

let actor = WasmActor::builder(engine, host)
    .component_from_path("plugins/temp-mapper.wasm")
    .epoch_deadline(1_000_000)
    .build()?;
```

The builder takes three things:

- An `Engine` (one per process is typical; `Engine::clone` is an `Arc`
  bump)
- A `WasmHost` — `DefaultHost` for the canonical world, or a custom host
  for richer capabilities (see [Host Extensibility](../architecture/host-extensibility.md))
- A component source — `component(Component)`, `component_from_path(path)`,
  or `component_from_bytes(Vec<u8>)`

Build the engine with:

```rust
let mut config = wasmtime::Config::new();
config.async_support(true);
config.wasm_component_model(true);
let engine = wasmtime::Engine::new(&config)?;
```

`build()` does the expensive setup *once* — compiles the component if
needed, then constructs the `Linker` against the host's `add_to_linker`.
At actor startup the linker is reused; the cost of wiring imports is paid
up-front, not per actor.

## The contract

Components built against the canonical `actor-component` world export the
`fuchsia:actor/actor` interface and import `fuchsia:actor/emit`:

```wit
interface types {
  variant payload-value {
    json(string),
    binary(list<u8>),
    empty,
  }

  record payload {
    %type: string,
    value: payload-value,
  }
}

interface actor {
  use types.{payload};

  record context {
    execution-id: string,
    node-id: string,
    task-id: string,
  }

  setup:    func(ctx: context) -> result<_, string>;
  handle:   func(ctx: context, msg: payload) -> result<_, string>;
  teardown: func(ctx: context) -> result<_, string>;
}

interface emit {
  use types.{payload};
  send: func(msg: payload) -> result<_, string>;
}
```

All three lifecycle exports are required. Stateless components implement
`setup` and `teardown` as no-ops returning `Ok(())`. Inbound messages are
typed `payload` values: a `%type` discriminator and a `payload-value`
variant (`json(string)`, `binary(list<u8>)`, or `empty`). Outbound
emissions flow through the host-imported `emit.send`, not through
`handle`'s return value — `handle` returns `Ok(())` once it's done
processing.

A component built with `wit-bindgen` (e.g., via `cargo-component`) ends up
writing something like:

```rust
impl exports::fuchsia::actor::actor::Guest for MyComponent {
  fn setup(ctx: exports::fuchsia::actor::actor::Context) -> Result<(), String> {
    fuchsia::log::log::log(fuchsia::log::log::Level::Info,
      &format!("setup node {}", ctx.node_id));
    Ok(())
  }

  fn handle(
    ctx: exports::fuchsia::actor::actor::Context,
    msg: fuchsia::actor::types::Payload,
  ) -> Result<(), String> {
    // msg.type_ is the event discriminator; msg.value carries the data
    let json_str = match msg.value {
      fuchsia::actor::types::PayloadValue::Json(s) => s,
      _ => "null".to_string(),
    };
    fuchsia::actor::emit::send(&fuchsia::actor::types::Payload {
      type_: "result".to_string(),
      value: fuchsia::actor::types::PayloadValue::Json(json_str),
    })
  }

  fn teardown(_ctx: exports::fuchsia::actor::actor::Context) -> Result<(), String> {
    Ok(())
  }
}
```

The `actor-component` world also imports `fuchsia:log/log` and
`fuchsia:http/outbound`, both of which `DefaultHost` provides.

## Actor lifecycle

When the orchestrator spawns the actor:

1. `Store<H::State>` is created via `host.initial_state(emitter)`. The
   `Emitter` is stashed in the state so the `emit` import callback can
   reach it.
2. The component is instantiated into the store via the pre-built
   `Linker`. This happens *once*; the bindings are reused for every
   subsequent lifecycle call.
3. `actor.setup(ctx)` is invoked. Use this to open connections, subscribe
   to streams, or perform discovery.
4. For each inbound message: the actor converts the inbox `Message` to a
   WIT `payload` and calls `actor.handle(ctx, msg)`. The component pushes
   any outgoing payloads via `emit::send` during the handle call.
5. On cancellation or inbox close, `actor.teardown(ctx)` is invoked
   before the store is dropped — long-lived resources (BLE handles, MQTT
   sessions) get a chance to close cleanly.

If anything fails — instantiation, a setup/handle trap, the component
returns an `Err(...)`, output JSON is malformed — the actor's `run` loop
records the error, runs teardown best-effort (logging any errors instead
of propagating), and returns the original error. `WorkflowHandle::join()`
will surface it in that actor's slot.

Cancellation is checked between `handle` invocations, not during. A
long-running `handle` call cannot be interrupted mid-flight; once it
returns, the runtime exits the loop and runs teardown. Wire up
`epoch_deadline` plus an epoch ticker if you need hard deadlines on
in-flight calls.

## What `DefaultHost` gives you

The off-the-shelf `DefaultHost` targets the canonical
`fuchsia:platform/actor-component` world. It satisfies:

- `fuchsia:log/log` — routes calls to `tracing` under target
  `"wasm.component"`. The actor's tokio task is already wrapped in a span
  carrying the `node` id, so log events automatically pick up context.
- `fuchsia:http/outbound` — delegates to the injected `HttpClient`.
- `fuchsia:actor/emit` — forwards typed payloads to the actor's outbound
  channel. Returns `Err("channel closed")` to the component if every
  downstream is gone; the component typically propagates that out of
  `handle`, which ends the actor cleanly.

WIT imports run as async host functions, so the bindings can await
directly on the underlying `HttpClient` and `Emitter::send` futures
without blocking the wasmtime worker.

For custom capabilities (MQTT, BLE, anything domain-specific), implement
your own `WasmHost` — see [Host Extensibility](../architecture/host-extensibility.md).

## Building test components

The integration test for `fuchsia-actor-wasm` needs a compiled component
on disk. Build it with [`cargo-component`]:

```bash
cd test-components/test-actor-component
cargo component build --release
```

This produces `target/wasm32-wasip1/release/test_actor_component.wasm`,
which the test loads via `component_from_path`.

[`cargo-component`]: https://github.com/bytecodealliance/cargo-component

## Performance notes

- **Component compilation is the expensive step.** Done once at
  `builder.build()` time.
- **Instantiation is once per actor**, not per message. With a persistent
  store, the cost of bringing up a wasm instance is amortized across the
  actor's lifetime.
- **Linker is built once** at `build()` and reused across all actor
  instances of this `WasmActor`.
- **`Engine::clone`** is an `Arc` bump; one engine for the whole process
  is the recommended shape.
- **No component caching across actors.** Each `WasmActor` instance
  holds its own `Component`. If you want shared compilation across many
  registered actor names, compile once and pass the `Component` directly
  into multiple builders.
