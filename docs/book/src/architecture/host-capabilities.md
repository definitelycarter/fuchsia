# Capabilities

A **capability** is something an actor can do *beyond* receiving a message and
emitting one — schedule a timer, write to durable state, call out over HTTP.
Fuchsia's stance: capabilities are **injected at construction, never ambient.**
An actor receives a typed bag of handles when it's built, stores the ones it
uses as fields, and can do exactly those things and no more. Its struct is the
statement of its authority.

## The capability bag

`ActorCapabilities` is a typed map handed to every `ActorCreator::create`:

```rust
fn create(&self, config: &ActorConfig, caps: &ActorCapabilities)
    -> Result<Box<dyn Actor>, ActorError>;
```

It's keyed by each capability's trait-object type, so a lookup returns the trait
object, never a concrete impl. An actor pulls only what it needs:

```rust
struct Debounce { emit: Arc<dyn Emit>, schedule: Arc<dyn Schedule>, /* … */ }

impl ActorCreator for DebounceCreator {
    fn create(&self, config: &ActorConfig, caps: &ActorCapabilities)
        -> Result<Box<dyn Actor>, ActorError>
    {
        Ok(Box::new(Debounce {
            emit: caps.emit(),
            schedule: caps.schedule(),
            /* … config from `config.settings` … */
        }))
    }
}
```

Fuchsia ships exactly **two** universal capabilities in the core, contributed by
two different layers. Anything else is a *domain* capability a product inserts
into the same bag under its own trait type (see [below](#domain-capabilities-live-in-products-not-the-core)).

### `emit` — route output downstream

```rust
pub trait Emit: Send + Sync { fn emit(&self, msg: Message); }
```

Provided by the **engine**. `caps.emit()` returns a sink that, on `emit`, looks
up this actor's successors in the routing table and delivers a clone to each.
The actor never names a neighbor — emission is fire-and-forget and infallible
(a full downstream mailbox sheds; see [Runtime & Engine](./engine.md)). If no
sink was wired, `emit()` falls back to a no-op, so an actor can always emit.

`emit` is **synchronous** — it's a non-blocking channel `offer`. The Wasm and
Lua hosts drive their guests via async wasmtime / mlua, but keeping `emit` a
synchronous offer keeps the emit path cheap on the way out.

### `schedule` — a delayed message to self

```rust
pub trait Schedule: Send + Sync {
    fn schedule_self(&self, after: Duration, msg: Message);
}
```

Provided by the **runtime**. `schedule_self` arms a timer that delivers `msg`
back into *this* actor's own mailbox after `after`, where it's handled like any
message. Fire-and-forget (no cancellation handle): time-based operators
re-arm on each input and ignore stale fires by tagging them. Falls back to a
no-op if unwired.

## Who contributes what

| Capability | Trait      | Contributed by            | Fallback if unwired |
|------------|------------|---------------------------|---------------------|
| `emit`     | `Emit`     | the engine (`RoutedEmit`) | no-op sink          |
| `schedule` | `Schedule` | the runtime (`TokioSchedule`) | no-op timer     |

The split mirrors the layers: the engine owns routing, and the runtime owns the
mailbox a timer fires into.

## Logging is `tracing`, not a capability

There's no `Log` capability. Native actors call `tracing::info!` etc. directly;
the runtime wraps each actor's task in a span carrying the `node` id, and parents
each handle span by the upstream's, so events pick up context automatically. The
host installs whatever subscriber it wants (stdout, JSON, OTLP). `tracing` *is*
the abstraction; there's no choice to make at a trait layer.

A Wasm or Lua *guest* can't call `tracing` directly — if a product wants guests
to log, it exposes a log import through its own host (see
[Host Extensibility](./host-extensibility.md)). The shipped base hosts wire only
`emit`.

## Domain capabilities live in products, not the core

Fuchsia does **not** ship a state sink, HTTP, KV, MQTT, or any domain capability,
and there is no `fuchsia-capabilities` crate. The core capability set is just
`emit` / `schedule` — both synchronous, both host-agnostic — and the capability
bag is **open**: a product adds whatever else it needs under its own trait type.

Everything else is a product concern:

- For a **native** actor, a capability is just an `Arc<dyn YourTrait>` the
  product puts into the bag (`ActorCapabilities::insert`) and the actor pulls
  with `caps.get::<dyn YourTrait>()`. A Home-Assistant-style product's
  state-write sink (`StateSink`) is exactly this — defined in the product, never
  in fuchsia.
- For a **Wasm or Lua** actor, the capability is an *import* the product wires
  into its own `WasmHost` / `LuaHost`, exposed through a product-defined WIT
  world or Lua global.

This keeps the runtime small and the import set open: a home-automation product
and an n8n-style product expose entirely different capability surfaces on the
same engine. See [Host Extensibility](./host-extensibility.md).
