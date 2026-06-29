# RFC: `from_fn` Actors & Default Lifecycle

> **Status: proposed.** Tracked in the [roadmap](../reference/roadmap.md#features)
> Features table until it lands.

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

```rust
// Stateless: a pure handle.
let node = fuchsia_actor::from_fn(|ctx, msg, emit| async move {
    emit.emit(msg);
    Ok(())
});

// Stateful: state owned by the adapter, handed to the closure as `&mut S`.
let node = fuchsia_actor::from_fn_with_state(0u64, |count, ctx, msg, emit| async move {
    *count += 1;
    emit.emit_to("out", msg);
    Ok(())
});
```

The open mechanics (see [Open questions](#open-questions)):

- **`emit` access.** An actor's `emit` capability is injected at *construction*, not
  available to a free-standing closure. So `from_fn` either (a) takes `emit` as a
  closure argument (shown above — the adapter holds the `Arc<dyn Emit>` it was built
  with and passes it in), or (b) the closure captures it. Option (a) keeps the
  closure free of capture ceremony and is the proposed shape.
- **Async-closure bounds.** Roughly `F: FnMut(&mut S, &ActorContext, Message, Arc<dyn Emit>)
  -> Fut`, `Fut: Future<Output = Result<(), ActorError>> + Send`. The wrinkle is the
  borrow of `&mut S` (and `&ActorContext`) across the returned future's `.await`
  points — the classic pre-async-closure lifetime friction. Passing `ActorContext`
  *by value* (it is three small `String`s) sidesteps the context borrow and ties into
  the roadmap's [`ActorContext` ids as `Arc<str>`](../reference/roadmap.md#open-questions)
  question.

### 3. `register_fn` — a closure as a node type

The version that earns its keep for products: register native closure logic as a
graph node type, no `ActorCreator` impl. Because a node type is built per instance
*with its capabilities*, the registration closure receives the per-instance
construction inputs (`ActorConfig` + `ActorCapabilities`) and returns the actor:

```rust
factory.register_fn("tap", |config, caps| {
    let emit = caps.emit();
    from_fn(move |ctx, msg, _emit| {
        let emit = emit.clone();           // refcount bump
        async move { emit.emit(msg); Ok(()) }
    })
});
```

This is sugar over a generic `FnCreator { f }` implementing `ActorCreator` — the
factory/runtime path is unchanged, so a `register_fn` node validates ports, routes,
and tears down like any other. `output_ports` defaults to `Dynamic` (a closure's
ports are whatever it emits on), with an optional builder to declare `Fixed` ports
for editor/validation use.

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

- **Stateful-closure ergonomics.** `from_fn_with_state` vs an `FnMut` capturing its
  own state vs requiring `'static` owned futures — which spelling stays ergonomic
  without `async` closures? Worth a small spike against the current `async_trait`
  setup before committing to a signature.
- **Scope: `Actor::from_fn` vs `register_fn`.** Ship only the test/example adapter
  (§2), or go all the way to closure node types (§3)? §3 is the product-facing win but
  the larger surface.
- **`ActorContext` by value or by ref in the closure.** By-value avoids the
  cross-`await` borrow but copies the ids per message; ties to the
  [`Arc<str>` ids](../reference/roadmap.md#open-questions) decision.
- **Naming.** `from_fn` / `from_fn_with_state` / `register_fn`, or a single builder?
  Match Rust std/`tower`-style `from_fn` conventions.
- **Sequencing.** §1 touches `actor.rs` and is independent; land it on its own (it
  also de-noises the existing builtins) before the larger §2/§3 design.
