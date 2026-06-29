# RFC: `from_fn` Actors & Default Lifecycle

> **Status: implemented.** All three parts shipped in `fuchsia-actor`: the
> `Actor` trait's default no-op `setup`/`teardown` (and the existing builtins
> de-noised to just `handle`), the `from_fn` / `from_fn_with_state` adapters
> (`from_fn.rs`), and `ActorFactory::register_fn` / `register_fn_with_ports` over
> a generic `FnCreator` — routed end to end through the engine. The open
> mechanics resolved as proposed: the adapter holds the `Arc<dyn Emit>` and
> passes it to the handler, `ActorContext` is passed by value, and the handler
> returns a `'static` future (state mutation is synchronous, before the future).
> Stands as the durable design record.

## Concept

Let a native actor be written as **just a `handle`** — no struct, no `impl`, no
empty lifecycle methods — when that is all it is. Two layered ergonomics:

1. **Default no-op `setup`/`teardown`** on the `Actor` trait, so a hand-written
   actor that has no lifecycle implements only `handle`.
2. **`from_fn`** — turn a closure into an `Actor`, and `register_fn` — register a
   closure directly as a graph **node type**, so glue logic needs none of the
   struct + `ActorCreator` + `impl Actor` triple.

A node still routes through the graph and emits through its injected `emit`
capability exactly as today; this changes only how the *handler code* is spelled.

## Motivation

Every native actor today hand-writes the full lifecycle even when two of the three
methods are empty. The `if`/`switch` builtins, `passthrough`, and most test sinks
all carry the same boilerplate:

```rust
#[async_trait]
impl Actor for Passthrough {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> { Ok(()) }
    async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
        self.emit.emit(msg);
        Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> { Ok(()) }
}
```

The `setup`/`teardown` pair is pure noise here. And for genuinely trivial logic — a
test fixture, an example wiring, a one-line host-embedded glue node — even the
`struct` + `ActorCreator` + `impl Actor` ceremony outweighs the behaviour. The
contract is *handle-per-message*; the common case should read that way.

This pairs with the same default-method move already made elsewhere in the contract
([`Emit::emit`] delegating to `emit_to`, [`ActorCreator::output_ports`] defaulting to
`Dynamic`) — lifecycle is the remaining piece of required-but-usually-empty surface.

[`Emit::emit`]: ./output-ports.md
[`ActorCreator::output_ports`]: ./output-ports.md

## Design

All of this lives in **`fuchsia-actor`** (the contract crate) — the trait, the
adapter, and the factory helper. No other layer changes; the runtime builds and
drives these actors exactly as it does any other `Box<dyn Actor>`.

### 1. Default no-op lifecycle (`fuchsia-actor`)

Give `setup`/`teardown` default bodies on the trait. Backward-compatible — existing
actors that implement them keep overriding; new ones may omit both:

```rust
#[async_trait]
pub trait Actor: Send + 'static {
    async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError>;

    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> { Ok(()) }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> { Ok(()) }
}
```

This is the small, uncontroversial half — it removes the boilerplate above on its
own and could land first. The only judgement call is philosophical (should lifecycle
be invisible by default?); every comparable actor framework defaults it to no-op.

### 2. `from_fn` — a closure as an `Actor`

An adapter that wraps a handler closure as a `Box<dyn Actor>`, with the lifecycle
defaulted to no-op. The model is stateful (`handle` is `&mut self`), so the useful
form carries state the closure mutates:

The adapter holds the `Arc<dyn Emit>` it was built with and hands a clone to the
handler each call, so the closure just names an `emit` argument. As-shipped:

```rust
// Stateless: a pure handle. `emit` is the sink the adapter was built with.
let node = fuchsia_actor::from_fn(emit, |ctx, msg, emit| async move {
    emit.emit(msg);
    Ok(())
});

// Stateful: state owned by the adapter, handed to the closure as `&mut S`. The
// handler returns a `'static` future, so state mutation is synchronous — done in
// the closure body, before the future the adapter awaits.
let node = fuchsia_actor::from_fn_with_state(0u64, emit, |count, ctx, msg, emit| {
    *count += 1;
    let seq = *count;
    async move {
        emit.emit_to("out", msg);
        let _ = seq;
        Ok(())
    }
});
```

The open mechanics, resolved (see [Open questions](#open-questions)):

- **`emit` access.** An actor's `emit` capability is injected at *construction*, not
  available to a free-standing closure. Resolved with option (a): `from_fn` takes the
  `Arc<dyn Emit>`, the adapter holds it, and passes a refcount-bumped clone to the
  handler as an argument — the closure stays free of capture ceremony. (The RFC's
  earlier sketch elided this constructor argument; the adapter must hold the sink to
  pass it in.)
- **Async-closure bounds.** As-shipped: `F: FnMut(&mut S, ActorContext, Message,
  Arc<dyn Emit>) -> Fut + Send + 'static`, `Fut: Future<Output = Result<(),
  ActorError>> + Send + 'static`. The `'static` future cannot borrow `&mut S` or
  `&ActorContext` across its `.await` points — the classic pre-async-closure lifetime
  friction. Two moves sidestep it cleanly without the HRTB-closure inference problems
  a borrow-across-await (boxed-future) bound would hit: `ActorContext` is passed *by
  value* (three small `String`s — ties into the roadmap's
  [`ActorContext` ids as `Arc<str>`](../reference/roadmap.md#open-questions) question),
  and any state mutation happens synchronously in the closure body before the future
  is produced. Since fuchsia's `emit`/`schedule` are synchronous, glue handlers rarely
  need to hold state across an await anyway.

### 3. `register_fn` — a closure as a node type

The version that earns its keep for products: register native closure logic as a
graph node type, no `ActorCreator` impl. Because a node type is built per instance
*with its capabilities*, the registration closure receives the per-instance
construction inputs (`ActorConfig` + `ActorCapabilities`) and returns the actor:

```rust
factory.register_fn("tap", |config, caps| {
    from_fn(caps.emit(), |ctx, msg, emit| async move {
        emit.emit(msg);
        Ok(())
    })
});
```

This is sugar over a generic `FnCreator` implementing `ActorCreator` — the
factory/runtime path is unchanged, so a `register_fn` node validates ports, routes,
and tears down like any other. `output_ports` defaults to `Dynamic` (a closure's
ports are whatever it emits on); `register_fn_with_ports(name, ports, builder)`
declares `Fixed` ports for editor/validation use. `FnCreator` is public
(`FnCreator::new` / `FnCreator::with_ports`), so a closure node can also be handed
straight to `engine.register` — which is how the end-to-end engine test wires one.
The `builder` is infallible (`Fn(&ActorConfig, &ActorCapabilities) -> Box<dyn
Actor>`); a closure that needs to fail at construction (e.g. parsing `settings`)
still uses a hand-written `ActorCreator`.

## Alternatives considered

- **Status quo — hand-write `struct` + `ActorCreator` + `impl Actor`.** Explicit and
  uniform, but the ceremony dominates for trivial nodes and every actor pays the
  empty-lifecycle tax. Right for substantial stateful actors; wrong as the only path.
- **Default lifecycle only (ship §1, drop §2/§3).** Removes the empty-method noise
  with near-zero risk and no new surface — a legitimate minimal version. It does not
  remove the `struct`/`impl`/creator ceremony, which is the larger win for glue and
  tests. §1 is a strict subset, so this is really "how far to go," not a fork.
- **A derive/attribute macro** (`#[actor] async fn handle(...)`). Hides the most
  boilerplate but adds a proc-macro dependency and a layer of magic over a trait that
  is deliberately small; a plain `from_fn` is more honest and debuggable. Rejected for
  now.

## Open questions

All resolved as implemented:

- **Stateful-closure ergonomics.** *Resolved:* `from_fn_with_state(init, emit,
  handler)` with `handler: FnMut(&mut S, …) -> Fut` and `Fut: 'static`. The
  `'static` future can't retain the `&mut S` borrow, so state mutation is
  synchronous (in the closure body, before the future). This compiles cleanly for
  closure literals — the alternative (a `for<'a> FnMut(&'a mut S, …) -> Pin<Box<dyn
  Future + 'a>>` bound that allows borrowing state across the await) hits the
  well-known HRTB-closure inference friction.
- **Scope: `Actor::from_fn` vs `register_fn`.** *Resolved:* shipped both §2 and §3.
- **`ActorContext` by value or by ref in the closure.** *Resolved:* by value, to
  avoid the cross-`await` borrow; still ties to the
  [`Arc<str>` ids](../reference/roadmap.md#open-questions) decision (which would make
  the per-message copy a refcount bump).
- **Naming.** *Resolved:* `from_fn` / `from_fn_with_state` (free functions, std/
  `tower`-style) and `ActorFactory::register_fn` / `register_fn_with_ports`.
- **Sequencing.** §1 landed first (it de-noises the builtins on its own), then the
  §2/§3 surface — all in one effort here.
