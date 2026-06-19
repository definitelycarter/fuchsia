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

Three capabilities ship in the core, contributed by three different layers.

### `emit` — route output downstream

```rust
pub trait Emit: Send + Sync { fn emit(&self, msg: Message); }
```

Provided by the **engine**. `caps.emit()` returns a sink that, on `emit`, looks
up this actor's successors in the routing table and delivers a clone to each.
The actor never names a neighbor — emission is fire-and-forget and infallible
(a full downstream mailbox sheds; see [Runtime & Engine](./engine.md)). If no
sink was wired, `emit()` falls back to a no-op, so an actor can always emit.

`emit` is **synchronous** — it's a non-blocking channel `offer`. This is what
lets the Wasm and Lua hosts drive their guests with no async bridge.

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

### `state` — a pre-scoped write sink

```rust
pub trait StateSink: Send + Sync {
    fn write(&self, value: Bson) -> Result<(), ActorError>;
}
```

Provided by the **host / provisioner**. The host hands the actor a sink already
scoped to its target (backend, collection, key); the actor calls `write` and
never learns where the value lands — same neighbor-ignorance as `emit`.
Pre-scoping is what makes partitioning safe: the actor *can't* write outside its
entity's storage because the sink doesn't permit it. Unlike `emit`/`schedule`
there's **no no-op fallback** — silently dropping a state write would hide a
misconfiguration, so an actor that needs it (the [`commit`](../runtimes/builtins.md)
builtin) fails construction when it's absent.

## Who contributes what

| Capability | Trait      | Contributed by            | Fallback if unwired |
|------------|------------|---------------------------|---------------------|
| `emit`     | `Emit`     | the engine (`RoutedEmit`) | no-op sink          |
| `schedule` | `Schedule` | the runtime (`TokioSchedule`) | no-op timer     |
| `state`    | `StateSink`| the host / provisioner    | **none** (returns `None`) |

The split mirrors the layers: the engine owns routing, the runtime owns the
mailbox a timer fires into, and only the host knows where state lives.

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

Fuchsia does **not** ship HTTP, KV, MQTT, or any domain capability, and there is
no `fuchsia-capabilities` crate. The core capability set is `emit` / `schedule`
/ `state` — the three a dataflow conditioning pipeline genuinely needs, all
synchronous, all host-agnostic.

Everything else is a product concern:

- For a **native** actor, a capability is just an `Arc<dyn YourTrait>` the
  product puts into the bag (`ActorCapabilities::insert`) and the actor pulls
  with `caps.get::<dyn YourTrait>()`.
- For a **Wasm or Lua** actor, the capability is an *import* the product wires
  into its own `WasmHost` / `LuaHost`, exposed through a product-defined WIT
  world or Lua global.

This keeps the runtime small and the import set open: a home-automation product
and an n8n-style product expose entirely different capability surfaces on the
same engine. See [Host Extensibility](./host-extensibility.md).
