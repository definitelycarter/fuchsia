# WebAssembly Actors

`fuchsia-actor-wasm` hosts WebAssembly components as Fuchsia actors. A
`WasmActor<H>` drives a component's `fuchsia:actor` lifecycle on the
handle-per-message model: the runtime owns the receive loop and calls the
actor's synchronous `setup` / `handle` / `teardown`, each of which trampolines
into the component. One `wasmtime::Store` is built per actor in `setup` and
reused for every `handle` — connections and state opened in the component's
`setup` stay live across messages, until `teardown`.

## Synchronous by design

The `fuchsia:actor` contract — lifecycle plus the host-imported `emit` — is
entirely synchronous (`emit` is a non-blocking channel `offer`). So this crate
uses **synchronous** wasmtime: no `async_support`, no `block_on`, no fibers. A
component call runs to completion on the runtime task driving it, exactly like a
native actor's `handle`. Capabilities that are inherently async (HTTP, …) are
*not* part of this contract — a product host wires them into its own `WasmHost`
and decides how to bridge (see [Host Extensibility](../architecture/host-extensibility.md)).

## Generic over the import set

fuchsia owns only the `fuchsia:actor` contract; it does not prescribe which
capabilities a component may import. `WasmActor` is generic over
[`WasmHost`](../architecture/host-extensibility.md) — the seam a product
implements to wire its own world's imports. The crate ships **`BaseHost`**,
which satisfies nothing but the contract: it links `emit` and traps any other
import the component carries.

```rust
pub struct WasmActor<H: WasmHost> { /* engine, component, host, emit, store, bindings */ }
```

The engine, component, host, and emit handle are captured when the actor is
built; the `Store` and bindings are created in `setup` and reused thereafter.

## The creator: one per runtime

A `WasmActorCreator<H>` is registered once under the type name `"wasm"`, not once
per component. It owns a catalog of compiled components keyed by id; each
`create` reads the component id from `ActorConfig.env` (under `COMPONENT_ENV_KEY`,
`"component"`) and builds an actor wired to the caller-supplied `emit`.

```rust
use fuchsia_actor_wasm::{BaseHost, WasmActorCreator};

let creator = WasmActorCreator::new(BaseHost::new())?     // builds a component-model Engine
    .with_path("temp-mapper", "components/temp-mapper.wasm")?;

engine.register("wasm", creator).await;
```

- `new(host)` builds a component-model `Engine`; `with_engine(engine, host)`
  shares one engine across creators or applies a custom `Config` (fuel, epochs).
- `with_path` / `with_bytes` (and the `insert_*` mutating forms) compile a
  component and register it under an id; `insert_component` registers an
  already-compiled `Component`.

A product swaps `BaseHost` for its own host to expose richer imports — same
creator, same resolution logic.

## The contract

Components target a world that exports `fuchsia:actor/actor` and imports
`fuchsia:actor/emit`. The canonical interfaces (in `wit/`):

```wit
// fuchsia:actor — types.wit
interface types {
  variant payload-value { json(string), binary(list<u8>), empty }
  record payload { %type: string, value: payload-value }
}

// actor.wit
interface actor {
  use types.{payload};
  record context { execution-id: string, node-id: string, task-id: string }
  setup:    func(ctx: context) -> result<_, string>;
  handle:   func(ctx: context, msg: payload) -> result<_, string>;
  teardown: func(ctx: context) -> result<_, string>;
}

// emit.wit
interface emit {
  use types.{payload};
  send: func(msg: payload) -> result<_, string>;
}
```

All three lifecycle exports are required; stateless components make `setup` /
`teardown` no-ops returning `Ok(())`. The `payload` value mirrors `MessageValue`
exactly — `json(string)` / `binary(list<u8>)` / `empty`. Outbound emissions go
through the host-imported `emit.send`, **not** through `handle`'s return value;
`handle` returns `Ok(())` once it's done.

`wit/` ships only `fuchsia:actor` — there is **no bundled platform world**. A
component's world is the product's (or, for a contract-only component, a tiny
world that just re-exports `actor` and imports `emit`).

### A component, with `cargo-component`

```rust
use bindings::exports::fuchsia::actor::actor::{Context, Guest, Payload};
use bindings::fuchsia::actor::emit;
use bindings::fuchsia::actor::types::PayloadValue;

struct Component;

impl Guest for Component {
    fn setup(_ctx: Context) -> Result<(), String> { Ok(()) }

    fn handle(ctx: Context, msg: Payload) -> Result<(), String> {
        let inner = match msg.value {
            PayloadValue::Json(s)   => s,
            PayloadValue::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            PayloadValue::Empty     => "null".into(),
        };
        let body = format!("{{\"echoed\": {inner}, \"node\": \"{}\"}}", ctx.node_id);
        emit::send(&Payload { type_: "echo".into(), value: PayloadValue::Binary(body.into_bytes()) })
    }

    fn teardown(_ctx: Context) -> Result<(), String> { Ok(()) }
}

bindings::export!(Component with_types_in bindings);
```

## Actor lifecycle

When the runtime builds and starts the actor:

1. A `Linker` is prepared. If `trap_unknown_imports()` is on (the default), every
   component import is first defined as a trap on the empty linker, then the host
   wires the real `emit`/contract imports on top (shadowing) — so unused WASI a
   guest drags in instantiates fine, while *calling* an unwired import fails
   loudly, and a real import is never clobbered by a trap.
2. `Store<H::State>` is created via `host.initial_state(emit)`, stashing the emit
   sink where the `emit` import callback can reach it.
3. The component is instantiated into the store once; the bindings are reused for
   every lifecycle call.
4. `setup(ctx)` runs (synchronously, inside the runtime's spawn).
5. Per message: the `Message` is converted to a WIT `payload` and `handle(ctx,
   msg)` is called; the component pushes emissions via `emit::send` during it.
6. On mailbox close, `teardown(ctx)` runs before the store is dropped. Teardown
   errors are logged, not propagated.

A `handle` call runs to completion — it isn't interrupted mid-flight. For hard
deadlines, build the creator's engine with epochs/fuel and a ticker.

## `BaseHost`

`BaseHost` wires only the `fuchsia:actor` contract: it links `emit` (forwarding
the component's emissions into the actor's `Emit` sink) and traps everything
else. It deliberately provides no log/http/WASI — those belong to product hosts.
It's enough to run any component that imports only `fuchsia:actor`. For richer
capabilities, implement your own `WasmHost`.

## Building a test component

The integration test loads a compiled component from disk; build it first with
[`cargo-component`]:

```bash
cd test-components/actor-echo && cargo component build --release
```

This produces `target/wasm32-wasip1/release/actor_echo.wasm`, which the test
registers via `WasmActorCreator::with_path`. The test
(`crates/fuchsia-actor-wasm/tests/wasm_actor.rs`) **skips** if the artifact is
absent, so `cargo test --workspace` stays green without the wasm toolchain step.

[`cargo-component`]: https://github.com/bytecodealliance/cargo-component

## Performance notes

- **Compilation is the expensive step** — done once when a component is inserted
  into the creator's catalog.
- **Instantiation is once per actor**, in `setup`, not per message.
- **`Engine` / `Component` clones are `Arc` bumps** — one engine per process (or
  per creator) is the recommended shape; share a `Component` across catalog
  entries if several ids back the same artifact.
