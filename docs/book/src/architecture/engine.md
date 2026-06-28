# Runtime & Engine

Three crates make up the execution core, bottom to top:

- **`fuchsia-transport`** — the bounded mailbox a message travels through, plus
  the ack that reports how handling it went.
- **`fuchsia-runtime`** — the actor substrate: it spawns one task per actor and
  owns the recv→handle→ack loop. Provides the `schedule` capability.
- **`fuchsia-engine`** — routing: it instantiates nodes as actors, keeps a live
  table of who feeds whom, and delivers each emission to its successors.
  Provides the `emit` capability.

Underneath them is `fuchsia-actor`, the contract.

## The contract (`fuchsia-actor`)

```rust
#[async_trait]
pub trait Actor: Send + 'static {
    async fn setup(&mut self, ctx: &ActorContext) -> Result<(), ActorError>;
    async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError>;
    async fn teardown(&mut self, ctx: &ActorContext) -> Result<(), ActorError>;
}
```

All three are **async** and take `&mut self`, so a `handle` can `.await` I/O
without blocking the runtime thread. The runtime guarantees a single task drives
one actor, so `&mut self` is sound without locking — even across an `.await` —
and an actor's state is just its fields. Handling stays sequential: one `handle`
is in flight per actor at a time.

A **`Message`** is a typed payload — a `type_` discriminator plus a value:

```rust
pub struct Message { pub type_: String, pub value: MessageValue }

pub enum MessageValue { Json(serde_json::Value), Binary(Vec<u8>), Empty }
```

An **`ActorContext`** is per-call identity threaded through every invocation —
`execution_id`, `node_id`, `task_id`. Capabilities are *not* on the context;
they're injected once at construction (see [Capabilities](./host-capabilities.md)).

Actors are built by an **`ActorCreator`**, registered into an `ActorFactory`
under a type name:

```rust
pub trait ActorCreator: Send + Sync + 'static {
    fn create(&self, config: &ActorConfig, caps: &ActorCapabilities)
        -> Result<Box<dyn Actor>, ActorError>;
}
```

`ActorConfig` carries the per-instance config, split by *who reads it*: `env`
(a `BTreeMap<String, String>`, host-curated) and `settings` (an opaque `bson`
document the actor deserializes its own typed view from).

An **`ActorId`** is a `group` plus a local `id` — the group namespaces ids
globally (and is the unit `remove_graph` tears down). It Displays as `group/id`
(or bare `id` for the default group).

## Mailboxes and acks (`fuchsia-transport`)

Every actor reads from a bounded **mailbox** — a tokio mpsc of `Delivery`:

```rust
pub struct Delivery { pub msg: Message, pub ack: Ack, pub span: Span }
```

A `Delivery` carries the message, an **ack** that reports the handling outcome
exactly once, and the **trace span** active where it was produced (so the
receiver's handle span can be parented by it — that's how a trace follows a
message across the task boundary).

The ack has two shapes:

- `Ack::Health(Arc<Health>)` — **at-most-once**: fold the outcome into shared
  `Health` counters (`handled` / `errored`). Used by pre-write producers that
  prefer to shed a stale reading rather than block.
- `Ack::Complete(oneshot::Sender<Outcome>)` — **at-least-once**: send the
  outcome back to a feeder. If the delivery is shed or the actor dies
  mid-handle, the dropped sender shows up as a closed channel, which the feeder
  treats as failure and retries — retry-on-loss for free.

There is deliberately **no `Transport` trait**. Actors always read a channel
mailbox; durability is layered *in front* by whoever feeds it. Sending is either
`offer` (non-blocking; a full mailbox yields `Offer::Shed`) or `send` (awaits
room, for producers that must not drop).

## The runtime loop (`fuchsia-runtime`)

`Runtime` registers creators and spawns actors. `spawn_with_caps` is the core:

1. Create the actor's mailbox and `Health` up front (so the scheduler can hold a
   weak handle back to this same mailbox — timers deliver there).
2. Layer the `schedule` capability into the bag (it needs the mailbox just
   created), then build the actor via its creator with the full bag.
3. Await `setup`. On failure the actor is dropped and nothing is
   registered.
4. `tokio::spawn` the recv loop and register an `ActorHandle` (id, type,
   mailbox, health) so callers can deliver to it.

The loop itself:

```rust
while let Some(delivery) = rx.recv().await {
    let Delivery { msg, ack, span: parent } = delivery;
    let span = tracing::debug_span!(parent: &parent, "actor.handle", …);
    let outcome = actor.handle(&ctx, msg).instrument(span).await;
    ack.report(outcome);
}
actor.teardown(&ctx).await;   // mailbox closed → drain → teardown
```

When every sender to the mailbox is dropped, `recv` returns `None`, the loop
ends, and `teardown` runs. `stop(id)` removes the handle (closing its sender)
to trigger exactly that.

### The `schedule` capability

`TokioSchedule` implements `Schedule::schedule_self(after, msg)`: it spawns a
timer that, on fire, **upgrades a weak handle** to the actor's own mailbox and
`offer`s the message back. Weak so a pending timer can't keep a torn-down actor
alive; the delayed message arrives through the normal `handle` path, tagged with
the actor's own health ack. Time-based operators (debounce) arm these on input.

## Routing (`fuchsia-engine`)

`Engine` wraps a `Runtime` and a live **routing table** (`RouterState`: a map of
each node's successors, and a map of every node's mailbox + health). All methods
take `&self`, so the engine is shared as `Arc<Engine>`.

- **`add_node(id, type_name, config, caps)`** — the engine adds the one
  capability it owns, `emit` (a `RoutedEmit` closed over this node's id and the
  shared table), spawns the actor through the runtime, and registers its mailbox
  as a routable target.
- **`add_edge(from, to)`** — records that `from`'s emissions flow to `to`.
- **`remove_graph(group)`** — stops every actor in a group and drops its edges.
  Scoped to the group; other graphs are untouched, and cross-group edges into it
  simply stop resolving (a graceful drop).
- **`push(entrypoint, msg)`** — offers an external event into one node's mailbox
  (best-effort, at-most-once; an unknown id is `NotFound`).
- **`push_durable(entrypoint, msg)`** — the at-least-once counterpart: an `async`
  ingress that *sends* (backpressure, not shedding) with an `Ack::Complete` and
  **awaits the handle outcome**. `Ok(())` means the node actually handled the
  message, so a durable caller (a leased queue worker) can delete its job; a
  handler error (`Handle`), a vanished mailbox (`Undelivered`), or a lost ack
  (`Lost`) are retriable. It awaits *delivery + outcome* — it does not persist;
  the queue, lease, and the caller-applied timeout/retry live above it, so
  entrypoints reached this way must be idempotent.

When an actor emits, `RoutedEmit` looks up the source's successors and `offer`s a
clone of the message to each successor's mailbox. The actor stays
neighbor-ignorant; the engine owns the addressing, and because the table is a
lookup (not baked wiring) graphs can come and go without re-instantiating actors.

### Topology semantics

These fall out of "mailbox per actor, routing table per engine":

- **Linear chain.** One successor; messages flow straight through.
- **Fan-out (one → many).** A node with several successors `offer`s a clone to
  each. Every downstream sees the same value.
- **Fan-in / merge (many → one).** Several nodes share one successor; its mailbox
  interleaves their emissions as they arrive. This is *merge*, not a synchronous
  join — "wait for one from each and combine" is a dedicated actor's job.
- **Shedding, not blocking.** Routing uses `offer`, so a full downstream mailbox
  **sheds** the delivery (at-most-once); its `Health` records the outcome. A slow
  consumer can't stall the whole graph — it loses readings instead, which for
  the conditioning path is the right trade.

## What the core deliberately doesn't do

- **No retry logic.** An at-least-once feeder retries on a dropped `Complete`
  ack; beyond that, an actor that wants to retry does so in `handle`.
- **No timeouts on `handle`.** A `handle` call runs to completion. If a node
  needs a deadline it uses `tokio::time` internally; the runtime won't interrupt
  it mid-call.
- **No dynamic routing.** Routing is the graph. An actor decides *what* to emit;
  the *set of edges* changes only via `add_edge` / `remove_graph`, not from
  inside `handle`.
- **No persistence of in-flight messages.** Mailboxes are in-memory. Durability
  is the feeder's job (the at-least-once ack), or a specific actor's (e.g. a
  product's state-writing terminal node).

These are intentional. The core is small so the actors can be specific.
