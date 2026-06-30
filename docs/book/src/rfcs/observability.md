# RFC: Observability — Correlation-Scoped Tracing

> **Status: partially implemented.** Shipped: the data-plane spans (`run` root on
> `push`/`push_durable`, `engine.route`, and `correlation` + `outcome` on
> `actor.handle`), the control-plane topology spans (`add_node` / `add_edge` /
> `remove_graph` / `restart_node`), the flow + failure events (`message.shed`,
> `emit.no_route`, `handle.retry`, `message.poisoned`, `dead_letter` via a shared
> `record_dead_letter` helper, `node.died`), a workspace `tracing` dependency, and
> the `fuchsia-examples` demo subscriber. **Deferred** (tracked in the roadmap
> Features row): `guest.handle` spans on the Wasm/Lua seam; completing the
> `node.*` lifecycle-event family (restarting / revived / rebuild_failed /
> teardown_failed / spawned / stopped — the supervisor still logs these ad hoc);
> the `attempt` field on `actor.handle` and the `dead_letter` event's
> reason-detail payload; `schedule.fire` re-entry; and the **no-run sentinel**
> (`Delivery::new` / schedule `unwrap_or_default` must yield a recognizable `none`,
> not a minted id — see the two-identities rule). Builds on
> [Per-Message Correlation Id](./message-correlation-id.md).

## Concept

Make a run observable end-to-end through [`tracing`](https://docs.rs/tracing)
spans **and events** keyed on the [`CorrelationId`](./message-correlation-id.md). A
trigger opens a **`run` span** carrying the correlation; every hop the message
takes — engine route, actor handle, guest call — opens a child span that inherits
the parent chain *and* records the correlation as a first-class field. *Within*
those spans, every state transition the runtime makes — a message shed, poisoned,
retried, dead-lettered; a node restarted, revived, or dead — fires a structured
event that inherits the same correlation. The result: a subscriber can reconstruct
the full causal tree of a run *and* see every loss and failure along it, and can
filter or group *all* of a runtime's activity by run id, without any actor or guest
touching tracing code.

The engine is the critical piece. Today it propagates spans and correlation but
emits no spans of its own — so the routing layer, where a message fans out across
edges, is a blind spot in the trace. This RFC gives the engine its own spans.

## Motivation

The substrate is already half-built and proven, but unusable as a correlation
view:

- **The span backbone exists.** Every [`Delivery`] carries the span active where
  it was produced (`pub span: Span`, captured via `Span::current()` in
  `Delivery::new`, `crates/fuchsia-transport/src/delivery.rs`). The runtime opens
  an `actor.handle` span per delivery, **parented by the upstream's span**, so a
  trace follows a message across the mailbox/task boundary —
  `tracing::debug_span!(parent: parent, "actor.handle", node = …, kind = …)` in
  `handle_once` (`crates/fuchsia-runtime/src/runtime.rs:869`). This is the thing
  `#[instrument]` alone *cannot* do: each actor runs on its own task, so an
  attribute macro would root every handle at the task spawn, not at the producing
  message. The cross-task parent chain is carried by the delivery, and
  `crates/fuchsia-engine/tests/tracing_propagation.rs` already asserts
  `ingress → a.handle → b.handle`.

- **But the correlation isn't on the spans.** The `actor.handle` span records
  `node` and `kind` — not the run id. So a subscriber can walk the parent chain of
  *one* trace it already has a handle on, but cannot answer "show me everything for
  `cid-7`" — the exact question [`CorrelationId`] exists to answer. The run id
  lives in the task-local and on `ActorContext::execution_id`, yet never reaches a
  span field.

- **The engine is invisible.** `RouterState::route`
  (`crates/fuchsia-engine/src/router.rs:292`) does the fan-out — resolve a port's
  successors, clone the message per edge, offer to each mailbox, tally
  delivered/shed/no-route counters — entirely without a span. `Engine::push` /
  `push_durable` (the triggers) open no root span either; tests wrap `push` in an
  ad-hoc `info_span!("ingress")` by hand. There is no engine-owned root, and no
  visibility into *which edge* a message took or *why* it was shed.

- **The failure machinery is rich but silent in a trace.** The runtime has a full
  failure taxonomy — `DeadLetterReason` (`Poison` / `Failed` / `RetryExhausted` /
  `NodeDied`), the dead-letter sink, the `Health` counters
  (`handled`/`errored`/`died`/`poisoned`/`crashed`). But the transitions that drive
  it **bump a counter and route to the sink while emitting no `tracing` event**:
  the poison gate (`crates/fuchsia-runtime/src/runtime.rs:527`), the fail /
  retry-exhausted dead-letters (`runtime.rs:690`, `:752`), the node-died drain
  (`crates/fuchsia-runtime/src/supervisor.rs:594`), and the routing sheds /
  no-routes (`crates/fuchsia-engine/src/router.rs`). So "why did *this* run lose a
  message?" can't be answered from a trace at all — only by polling aggregate
  counters that have already forgotten which run. Conversely the supervisor's
  ~12 lifecycle logs (`supervisor.rs:254/258/264/332/344/399/412/432/433/528`,
  `runtime.rs:951/953`) *do* fire, but as ad-hoc messages at mixed levels with no
  correlation field — they can't be grouped by run or read as a consistent family.

- **No conventions, no consumer.** Span names, fields, and levels are ad hoc
  (`actor.handle` at DEBUG; lifecycle `tracing::error!`/`warn!` scattered through
  the supervisor and the guest hosts). No crate declares a shared `tracing`
  version (each pins `"0.1"` independently), `tracing-subscriber` is only a
  dev-dependency of `fuchsia-engine`, and nothing demonstrates wiring a subscriber.
  The data is half-emitted and entirely unconsumed.

What this unblocks: per-run debugging ("trace this workflow execution"), the
foundation for metrics/trace export (the roadmap's open
[routing-counters-surface](../reference/roadmap.md#open-questions) question), and a
product-grade story for the n8n/Home-Assistant
[backbone](./runs-and-results.md) where "why did *this* run do *that*?" is the
core support question.

## Design

The span tree we want, for `a → b` triggered with `cid-7`:

```text
run{correlation=cid-7, entrypoint=a}        ← fuchsia-engine: Engine::push
└─ actor.handle{correlation=cid-7, node=a}  ← fuchsia-runtime: handle_once
   └─ engine.route{correlation=cid-7, source=a, port=out, fanout=1}
      └─ actor.handle{correlation=cid-7, node=b}
```

Each layer owns the spans for its concern; correlation appears as a field at every
level so spans are independently filterable by run, while the existing
delivery-carried parent chain keeps them causally linked across tasks.

That tree is the **data plane** — per-run spans keyed on the correlation. The
engine also has a **control plane**: the topology and lifecycle operations that
build and tear down the graph (`add_node`, `add_edge`, `remove_graph`,
`restart_node`). Those carry *no* correlation — there is no run when a graph is
assembled — and are keyed on graph identity (node / edge / group) instead.
Observability covers both planes; the correlation-keyed data plane is the
centerpiece, the control plane the complement.

Three primitives, used for what each is good at — they compose rather than compete:

- **Counters** — the always-on, near-free aggregates that already exist (`Health`'s
  `handled`/`errored`/`died`/`poisoned`/`crashed`, the engine's per-`(node, port)`
  `delivered`/`shed`/`no_route`). They answer *how many*.
- **Spans** — scopes/durations keyed on the correlation (`run`, `actor.handle`,
  `engine.route`, plus the control-plane topology spans). They answer *what
  happened, in what order, how long*.
- **Events** — point-in-time transitions fired *inside* a span (`message.shed`,
  `message.poisoned`, `dead_letter`, `node.died`, …). They answer *exactly what
  happened to this message/node, and why*.

Each transition typically does all three: bump the counter, fire the event, inside
the span. The counter is the dashboard, the event is the per-occurrence detail, the
span is the context that ties it to a run.

### Two identities, never conflated

The spans and events above carry **two distinct identifiers**, and the whole
discipline is keeping them separate:

1. **Structural trace identity** — the `tracing`/OTel span tree (`trace_id`,
   spans, parent / `follows_from` links). It is *universal*: **every** operation
   gets a span — data-plane, control-plane, a panic restart, a timer fire — and it
   needs zero domain knowledge. Synchronous containment is a parent span; an async
   or cross-task handoff is a `follows_from` link (the whole basis of the `run`
   root, `node.restart`, and the engine-decoupling work).
2. **Correlation id** — the run id. A *domain* identity meaning "this is the run
   caused by external event X". It is **not** on every span; it is a field that
   *rides on* spans **only where a run actually exists**, and is deliberately
   **absent** on work that is not part of a run. That absence is correct, not a gap.

The one trap is **fabricating a correlation id for work that has no run** — a
synthetic/default id on a control-plane op or a restart. It pollutes the run-space
(so "show me run X" returns infrastructure noise) and, for a product that uses the
correlation for action→state lineage, corrupts that lineage with a false cause.

**The decision rule** — this answers every "does this entrypoint take a
correlation id?":

| Kind of work | Correlation | Trace |
|---|---|---|
| **Run trigger** — external ingress (`push` / `push_durable` from a webhook / MQTT / scheduled job) | **mint** (or adopt an external id) | new `run` span |
| **Run continuation** — an `emit`, a `route`, a scheduled self-message that continues the run | **propagate** the in-scope id | child span |
| **Control-plane / lifecycle** — `add_node`/`add_edge`/`remove_graph`, a panic restart, a revival, a bring-up | **none** | span + `follows_from` to its cause |

The litmus: *is this work being done **on behalf of** a run?* If yes → propagate.
If it was merely **triggered by** one → **link, don't inherit** (a `follows_from`,
and optionally the causing run recorded as a `caused_by` *attribute* — never
adopted as the work's own id; e.g. an auto-restart can record the crashing
message's run as `caused_by` without becoming part of it). If neither → span only.
fuchsia already lands this: `push`/`push_durable` take a correlation;
`add_node`/`add_edge`/`remove_graph` and `node.restart`/`node.teardown` do not —
they are `follows_from`-linked instead.

**The no-run sentinel (a fix this rule demands).** Because fabrication is the trap,
the one place fuchsia can currently fabricate must be closed:
`CorrelationId::current().unwrap_or_default()` (in `Delivery::new` and the schedule
capability) **mints a fresh `cid-N` when no run is in scope** — so a detached timer
or a no-run construction produces a real-looking run id. The "no run in scope" case
must resolve to a recognizable **`none`** sentinel that run-queries filter out, not
a minted id, so non-run work can never masquerade as a run.

The standard, in one line: *trace everything (structural, universal,
`follows_from` for async/lifecycle); correlate only runs (mint at a trigger,
propagate through, absent on control-plane/lifecycle — link, never inherit). One id
is OTel trace context; the other is domain baggage — never let the second look
present where there is no run.*

### The identity ladder

Those two are the *kinds*; concretely fuchsia carries four ids, coarsest to
finest, plus the run id that rides only where there's a run:

| Id | Granularity | Rides on | Kind |
|---|---|---|---|
| `node` (`node_id`) | the **registration** — a stable graph node | every span (`node` field) | structural |
| `generation` | one **instance** — changes per (re)build | the lifecycle spans (`node.bringup`, `node.restart`) + `actor.handle` | structural |
| `invocation` (`invocation_id`) | one `handle` call — per message / attempt | `actor.handle` (≈ its span id) | structural |
| `correlation` (`execution_id`) | the **run** a message belongs to | `run`, `actor.handle`, events — **not** control-plane | domain |

The first three are **structural / lifecycle** identities — always present, no run
required; `correlation` is the **domain** run id, present only on run work (per the
rule above). `generation` (the instance id) and `invocation` — renamed from
`task_id`, which misleadingly read as the tokio task — are introduced by the
[Supervised Node Lifecycle](./supervised-bringup.md) RFC. Recording `generation` on
`actor.handle` is what lets a trace say *which instance* of a node handled a
message; on `node.bringup` / `node.restart` it says *which incarnation* is coming up
or rebuilding.

### `fuchsia-engine` — the root and the route (the critical piece)

**The `run` root span**, opened inside the trigger so every run is rooted and
correlation-tagged without the host remembering to wrap `push`:

```rust
#[tracing::instrument(
  name = "run",
  skip_all,                                  // never record `msg` (payload) or `self`
  fields(correlation = %id, entrypoint = %entrypoint),
  level = "info",
)]
pub fn push(&self, entrypoint: &ActorId, msg: Message, id: CorrelationId)
  -> Result<(), EngineError>
{
  // body unchanged: resolve target, offer the Delivery::with_correlation(...)
}
```

The trigger is the one place the correlation arrives as an **explicit argument**
rather than a task-local: `push` / `push_durable` / `push_durable_attempt` already
take `id: CorrelationId` (`crates/fuchsia-engine/src/engine.rs:347`). So the root's
field is `%id` straight from the parameter — no `CorrelationId::current()` read, no
scope to be inside — which is precisely why the `run` root belongs to the engine
and not to the host. (Contrast `engine.route` below, the one engine path that
*does* read the task-local, because its `Emit::emit_to(port, msg)` signature
carries no correlation.)

This works because `Delivery::with_correlation` (called in the body) captures
`Span::current()` — now the `run` span. `push` is synchronous and returns once the
message is *offered*, but the `Delivery` holds a clone of the span, so the root
stays alive across the hop and downstream `actor.handle` spans parent under it.
This is the same mechanism the `ingress` test span relies on — it just moves
ownership of the root from "whatever the host wraps `push` in" to the engine, keyed
on the run id. The same `#[instrument]` goes on `push_durable_attempt` (async; the
span then also spans the awaited outcome, so the at-least-once path's
delivered/handled/lost result is captured on the root). `push_durable` delegates,
so it inherits it.

**The `engine.route` span**, around the fan-out in `RouterState::route`:

```rust
fn route(&self, source: &ActorId, port: &str, msg: Message) {
  // ... resolve `successors` (unchanged) ...
  let span = tracing::trace_span!(
    "engine.route",
    correlation = tracing::field::Empty,     // filled below; avoids minting on miss
    %source, %port, fanout = successors.len(),
  );
  if let Some(c) = CorrelationId::current() {
    span.record("correlation", tracing::field::display(&c));
  }
  let _enter = span.enter();                  // route() is fully synchronous — no await

  for edge in successors {
    // ... offer Delivery::new(msg.clone(), ack); now parented by `engine.route` ...
  }
}
```

`route` is the hot path — synchronous, run under the router *read* lock, on the
emitting actor's task. So the span is **TRACE** (disabled near-free when no TRACE
subscriber is active; `successors.len()` and the field record only run when it is),
and a plain `.enter()` guard is sound because there is no `.await` between enter and
drop. The correlation comes from the task-local — `route` always runs inside the
emitting actor's `correlation.scope(...)`, so `CorrelationId::current()` is set;
recording into a pre-declared `Empty` field avoids `unwrap_or_default()`'s mint-on-
miss. With this span in place, each downstream `Delivery::new` captures
`engine.route` as its parent, so the trace shows *which port and edge* carried the
message, and the per-`(node, port)` `delivered`/`shed`/`no_route` counters gain a
span to hang a future metrics layer on.

**The topology & lifecycle spans (the control plane).** The graph-mutation methods
on `Engine` aren't part of any run — the host's assembler invokes them to build or
tear down a graph — so they carry no correlation and root as top-level spans keyed
on what they act on. `#[tracing::instrument]` fits them directly: each is a
self-contained call with no cross-task hop, the case the macro handles natively.

- `add_node` (`engine.rs:96`) — `fields(node = %id, r#type = type_name)`, **INFO**,
  `err`. The span wraps the async body, so it *times the actor's `setup`* (which may
  do I/O) and records an `EngineError` on the failure path — "node `x` (type `wasm`)
  set up in 18 ms" or "setup failed". The highest-value control-plane span.
- `add_edge` (`engine.rs:226`) — `fields(from = %from, port, to = %to)`, **DEBUG**,
  `err(level = "debug")`. Gives the O(V+E) acyclicity walk a home and records a
  *rejected* wiring (`EngineError::Cycle` / `UnknownPort`) at DEBUG — a cycle
  rejection is validation, not a fault, so it shouldn't cry ERROR. `add_default_edge`
  delegates to `add_edge`, so it needs no span of its own.
- `remove_graph` (`engine.rs:296`) — `fields(group, nodes = Empty)`, **INFO**;
  record the torn-down node count once `ids_in_group` resolves. Times the teardown
  of the whole group.
- `restart_node` (`engine.rs:173`) — `fields(node = %id, force)`, **INFO**, `err`;
  records which branch fired (revive / forced rebuild / rejected-already-running),
  the operator-facing complement to the supervisor's automatic restart logs.

`register` is a one-liner (insert a creator into a map) — a DEBUG event is enough,
no span. The pattern, on `add_node`:

```rust
#[tracing::instrument(
  name = "add_node",
  skip_all,                          // never record `config`/`caps`/`self`
  fields(node = %id, r#type = type_name),
  level = "info",
  err,                               // an EngineError (e.g. setup failure) on the Err arm
)]
pub async fn add_node(
  &self, id: ActorId, type_name: &str, config: &ActorConfig, caps: ActorCapabilities,
) -> Result<(), EngineError> {
  // body unchanged
}
```

`#[instrument]` on an async method instruments the *future* (not a held guard), so
the span correctly spans the `runtime.lock().await` + `setup().await` without a
`!Send` guard across an await. `r#type` is the raw identifier for the `type`
keyword; it renders as the field `type`.

### `fuchsia-runtime` — correlation on the handle span

The `actor.handle` span already exists and is correctly parented; extend its
fields so it's filterable by run and records the attempt + outcome:

```rust
let span = tracing::debug_span!(
  parent: parent,
  "actor.handle",
  correlation = %correlation,    // NEW — the run id as a field (Arc<str>, ~free)
  node = %ctx.node_id,
  kind = %msg.type_,
  attempt = msg_attempt,         // NEW — distinguishes a retry from a first try
  outcome = tracing::field::Empty,
);
// ... after handle resolves, span.record("outcome", …) on the Ok/Err/stop arm.
```

`correlation` is already in hand in `handle_once` (it's the `correlation`
parameter); recording it is an `Arc<str>` display, no allocation. The runtime's
existing lifecycle logs (the supervisor's restart/death `error!`/`warn!`/`info!`,
the actor-task-died logs) gain the correlation field where one is in scope.

### Events — the state transitions within the system

This is where "all of it" lives. An event is a `tracing::event!` fired *inside* the
active span, so it inherits the parent **and the `correlation` field for free** — a
subscriber filtering on a run sees the run's spans and its failure events in one
view. Each event sits beside an existing counter bump and/or sink call; the
recommendation is to route every transition through **one small helper** (per
transition) that bumps the counter, fires the event, and — where applicable — calls
the dead-letter sink, so the three can never drift apart.

The **happy path needs no events.** A message delivered and handled *is* the
`actor.handle` span; an emit routed *is* the `engine.route` span. Those already
carry the correlation, so a normal run is fully visible as its span tree. (A product
that wants raw flow can flip on a TRACE `message.received` / `message.emitted`,
default-off.) The events below are for what a span tree *can't* show on its own: the
losses and the failures.

**Flow events** — data plane, fired inside `engine.route` / `actor.handle`, so
`correlation` is implicit:

| Event | Level | Fields | Site today |
|-------|-------|--------|------------|
| `message.shed` | WARN | `source`, `port`, `to` | `route` `Offer::Shed` arm (`router.rs`); also `Engine::push` ingress shed — **net-new** (and finally surfaces the roadmap's uncounted-`push`-shed gap) |
| `emit.no_route` | DEBUG | `source`, `port` | `route` no-route arms (`router.rs:305`, `:328`) — **net-new** |
| `handle.retry` | DEBUG | `node`, `attempt` | `handle_with_policy` retry loop (`runtime.rs:711`) — **net-new** |

**Quarantine / dead-letter events** — the message is in hand, so each carries its
`correlation`:

| Event | Level | Fields | Site today |
|-------|-------|--------|------------|
| `message.poisoned` | WARN | `node`, `attempts`, `dead_lettered` | poison gate (`runtime.rs:527`–`539`) — counter bumped, **no event today** |
| `dead_letter` | WARN | `node`, `reason`, + the reason's payload (`attempts` / `error` / `restarts`) | the four `sink.dead_letter(…)` calls (`runtime.rs:527`, `:690`, `:752`; `supervisor.rs:594`) — **no event today** |

**Node-lifecycle events** — node-keyed; they carry `correlation` only when a message
is implicated (noted per row):

| Event | Level | Fields | Site today |
|-------|-------|--------|------------|
| `node.spawned` / `node.stopped` | INFO | `node` | `add_node` commit / `runtime.stop` — **net-new** |
| `node.handle_panicked` | ERROR | `node`, `attempt`, `correlation` (in-flight) | `supervisor.rs:528` (pairs `record_crash` at `:527`) — **regularize** |
| `node.restarting` | WARN | `node`, `restart` (count) | `supervisor.rs:332`, `:344` — **regularize** |
| `node.revived` / `node.force_restarted` | INFO | `node` | `supervisor.rs:399`, `:412` — **regularize** |
| `node.rebuild_failed` | ERROR | `node`, `error`, `phase` (setup/rebuild) | `supervisor.rs:254`, `:258`, `:264` — **regularize** |
| `node.died` | ERROR (panic) / WARN (abnormal exit) | `node`, `cause`, `restarts` | `runtime.rs:951`, `:953`; budget exhaustion → `NodeDied` drain — **regularize** |
| `node.teardown_failed` | WARN | `node`, `error` | `supervisor.rs:432`, `:433`; guest teardown (`wasm`/`lua` `actor.rs`) — **regularize** |
| `node.deregistered` | DEBUG | `node` | engine `on_death` seam (`engine.rs:68`) — the quiet consequence of `node.died` |

Roughly half regularize the ~12 scattered lifecycle logs into one named family with
stable levels and a `node` field; the other half (flow + dead-letter/poison) are
net-new — today those transitions only touch a counter. Names are
dotted-namespaced (`message.*`, `emit.*`, `node.*`, `dead_letter`) so a subscriber
can filter a whole family by target.

**The correlation-availability rule.** A failure event carries `correlation` when a
message is in hand — poison, dead-letter, retry, and the mid-handle panic all hold
the `Delivery`. A *pure* node death (the task died with no message in flight,
`runtime.rs:951`/`:953`) is node-keyed only; that is honest, not a gap. The
`NodeDied` drain is the bridge: as it dead-letters each surviving mailbox message
(`supervisor.rs:594`), each `dead_letter` event carries *that* message's
correlation — so even a node death is traceable per affected run.

**Ownership.** `fuchsia-transport` owns the data types (`Health`, `DeadLettered`,
the route counters) and may host the thin event-emitting helper next to them (it
already depends on `tracing` for the delivery span). The events themselves fire
where the transition is *decided*: `fuchsia-runtime` (poison, dead-letter, retry,
restart, death, teardown), `fuchsia-engine` (shed, no-route, deregister),
`fuchsia-actor-{wasm,lua}` (guest teardown). No contract change — actors never see
any of it.

### "Other places" — guests, schedule, restart

- **Guests (`fuchsia-actor-wasm` / `fuchsia-actor-lua`).** A guest call already
  runs inside the runtime's `actor.handle` span and `correlation.scope`, so its
  emits propagate correctly. Add a child `guest.handle` span (with `runtime =
  "wasm" | "lua"`) on the `WasmHost`/`LuaHost` seam so time spent *inside* the
  guest is distinguishable from runtime overhead, and so the existing teardown
  `tracing::warn!`s sit under the right run.
- **Schedule (`fuchsia-runtime`'s `TokioSchedule`).** A scheduled self-message
  fires later on a **detached timer task**, which has no live span — but
  `schedule.rs` already captures `CorrelationId::current()` at scheduling time. The
  fired emission should re-open a `schedule.fire` span keyed on that captured
  correlation (and re-enter the correlation scope, which it must already do for the
  emit to carry the run id), so a delayed/debounced emission still correlates to its
  originating run instead of starting a detached trace.
- **Restart (`fuchsia-runtime`'s supervisor).** `run_incarnation` already extracts
  and forwards `correlation` per delivery through the `catch_unwind`, so handle
  spans on a restarted incarnation keep their ancestry for free — no change beyond
  the field addition above.

### Field & level conventions

A small canonical vocabulary, documented once (a new `architecture/observability.md`
page) so every span and event speaks it:

| Field | Meaning | Source |
|-------|---------|--------|
| `correlation` | The run id — the primary grouping key (run work only) | `CorrelationId` / `ctx.execution_id` |
| `node` | The registration — stable graph node | `ActorId` / `ctx.node_id` |
| `generation` | Which instance (changes per (re)build) | the supervisor / lifecycle |
| `invocation` | One `handle` call (was `task_id`) | `ctx.invocation_id` |
| `port` | Named output port on an emit/route | route arg |
| `kind` | Message type (`msg.type_`) | `Message` |
| `attempt` | Per-message attempt (retry/re-delivery) | `Delivery::attempts` |
| `outcome` | `complete` / `shed` / `no_route` / `error` | recorded on close |
| `type` | Actor type name (`add_node`) | `add_node` arg |
| `from` / `to` | Edge endpoints (`add_edge`); `to` also the shed target | `add_edge` args / `route` |
| `group` | Graph group (`remove_graph`) | `remove_graph` arg |
| `reason` | Dead-letter reason (`poison`/`failed`/`retry_exhausted`/`node_died`) | `DeadLetterReason` |
| `cause` | Node-death cause (`panic`/`abnormal_exit`/`budget_exhausted`) | death seam |
| `restart` / `restarts` | Restart count (in progress / at permanent death) | supervisor |

The first six are the **data-plane** vocabulary (per-run, correlation-keyed).
`type` / `from` / `to` / `group` are **control-plane** fields on the topology spans,
which carry no `correlation`. The rest are **event** fields; an event carries
`correlation` whenever a message is in hand (see the correlation-availability rule).

| Level | Used for |
|-------|----------|
| ERROR | Panics & permanent failure: `node.handle_panicked`, `node.died` (panic), `node.rebuild_failed`, `add_node` setup failure |
| WARN | Recoverable loss & lifecycle churn: `message.shed`, `message.poisoned`, `dead_letter`, `node.restarting`, `node.died` (abnormal exit), `node.teardown_failed` |
| INFO | The `run` root; node/graph lifecycle (`node.spawned`/`stopped`/`revived`, `add_node`, `remove_graph`, `restart_node`) — low cardinality, safe to leave on |
| DEBUG | Per-message structure (`actor.handle`, `guest.handle`), edge wiring (`add_edge`), `emit.no_route`, `handle.retry`, `node.deregistered` |
| TRACE | The hottest paths (`engine.route` per edge; optional `message.received`/`emitted`) |

**Payloads are never recorded by default.** `skip_all` on the instrument macros and
the absence of any `MessageValue`/`payload` field is deliberate: a workflow
backbone routes PII and secrets, and message bodies are high-volume. A product that
wants payload capture opts in at its own layer.

### Ownership of the subscriber (the consumer)

Fuchsia is a library; products own `main`. So **fuchsia emits the spans/events and
ships the conventions, but installs no subscriber** in the core crates —
`tracing-subscriber`, sampling, and any OTel/export layer are the product's choice,
exactly as the host owns its `Engine` wiring. Concretely:

- Add a **workspace `tracing` dependency** in the root `Cargo.toml`
  (`tracing.workspace = true` across the crates) so the version is aligned in one
  place — today each crate pins `"0.1"` independently.
- `fuchsia-examples` grows a documented subscriber (a `tracing_subscriber::fmt`
  layer that prints the `run`/`handle`/`route` tree for the demo graph), as the
  copy-paste starting point.
- The existing `tracing_propagation.rs` custom layer stays as the test-side proof,
  extended to assert the `correlation` field is present at each level.

No new capability and no guest-contract change: observability rides the task-local
and the delivery's span, both of which already exist. `fuchsia-actor` (the
contract) is untouched — actors stay tracing-agnostic; `ctx.execution_id` is the
only correlation surface they ever need.

## Alternatives considered

- **Do nothing (status quo).** The parent chain works, but with no correlation
  field and no engine spans you can debug a trace you already caught, not *find*
  one by run id, and the routing layer stays dark. Rejected — it leaves the
  half-built substrate unusable for its actual purpose.
- **Structured logging keyed on correlation, no spans.** Emit `tracing::event!`s
  with a `correlation` field instead of spans. Simpler, but loses the causal tree
  and timing the span hierarchy gives — and we already *have* the span backbone, so
  this would be a step sideways. Rejected.
- **A bespoke event bus / observer capability.** Add an `Observe` capability to the
  bag and have actors/engine push typed events to a product sink. Far more
  invasive (touches the contract and every emit site), reinvents what `tracing`'s
  subscriber model already does, and couples actors to observation. Rejected;
  `tracing` *is* the pluggable-subscriber abstraction.
- **Bake in OpenTelemetry export.** Make the core depend on `opentelemetry` and
  export OTLP directly. Rejected for layering: OTel is one subscriber over the
  standard `tracing` data model — a product adds `tracing-opentelemetry` itself.
  Fuchsia owning it would force the dependency on every host.
- **`#[instrument]` everywhere, no delivery span.** Can't cross the task boundary —
  each actor is its own task, so the macro roots every handle at spawn, not at the
  producing message. The delivery-carried span is load-bearing; `#[instrument]` is
  only for within-task structure (the engine's `push`/`route`). This is a
  *combination*, not an alternative.

## Test plan

- Extend `tracing_propagation.rs`: assert the `run` root carries
  `correlation`, and that `correlation` is recorded on every `actor.handle` and
  `engine.route` span in the chain (not just the parent linkage).
- A fan-out test (one port → two edges) asserting two `engine.route`-parented
  `actor.handle` spans share one `correlation`.
- A shed test (full downstream mailbox) asserting the `message.shed` event *and*
  the `outcome=shed` field fire, and that the route counter still tallies — counter,
  span, and event agree.
- A poison test (re-deliver past `poison_after`) asserting a `message.poisoned` and
  a `dead_letter{reason=poison, attempts=N}` event fire carrying the right
  `correlation` — alongside the existing `DeadLettered`-on-sink assertions.
- A node-death test (panic a node out of its restart budget) asserting `node.died`
  fires node-keyed, and that each `dead_letter{reason=node_died}` from the drain
  carries *its* message's `correlation` (the per-run bridge).
- A bench guard (`cargo bench -p fuchsia-runtime`): confirm the per-handle field
  additions, the TRACE `engine.route` span, and the failure-path events are all
  near-free with **no** subscriber installed (the disabled-span/-event fast path),
  so neither the hot route path nor the failure paths regress.

## Open questions

- **Counters vs spans vs events (roadmap linkage).** The engine and transport
  already keep in-process counters (`delivered`/`shed`/`no_route`,
  `handled`/`errored`/`died`/`poisoned`/`crashed`). Do those graduate to a metrics
  export, or does a future `tracing`-metrics layer derive them from the span/event
  fields? Proposed stance: counters stay as the always-on cheap gauges; spans +
  events are the opt-in deep view; a future OTel layer bridges both. This RFC
  resolves the roadmap's
  [routing-counters-surface](../reference/roadmap.md#open-questions) question only
  as far as "spans/events are the trace surface; export is a later layer."
- **Where the event helper lives.** The per-transition helper (bump counter + fire
  event + maybe call sink) could live in `fuchsia-transport` next to `Health` /
  `DeadLettered` — keeping the three co-located and impossible to drift — or each
  crate could emit at its own site. The former pulls a little event vocabulary into
  transport (already a `tracing` dependant); the latter keeps transport as pure
  data types. Leaning toward a thin transport helper for the counter+event pair, with
  the sink call staying at the runtime site that owns the policy.
- **Schedule re-entry.** Confirming `TokioSchedule` re-enters the correlation scope
  (not just captures it) on fire is a prerequisite for `schedule.fire` to correlate
  — needs a check against the current `schedule.rs` and possibly a small fix.
- **Correlation field name.** `correlation` (fuchsia's own term, matching
  `ctx.execution_id`) vs an OTel-friendly `trace_id`. Leaning `correlation` to stay
  truthful to the model; a `tracing-opentelemetry` layer can map it.
- **Sampling at scale.** A high-throughput graph at TRACE produces an
  `engine.route` span per edge per message. Left to the product's subscriber
  (per-level filtering / sampling); fuchsia just keeps the hot path disabled-cheap.

## Follow-ups

- Decide whether `Engine::push` returning the offer outcome (the roadmap's
  uncounted-`push`-shed gap) should also be the `outcome` field on the `run` span —
  the two are the same information and could land together.
- **No-run sentinel.** Replace the `CorrelationId::current().unwrap_or_default()`
  fabrication in `Delivery::new` and the schedule capability with a recognizable
  `none` (so a detached/no-run construction never looks like a run), and make
  run-queries filter it. Settle the representation — a reserved string (`"-"`,
  `"none"`) vs. an `Option<CorrelationId>` on the no-run paths — so the wire/field
  form is unambiguous.
- **`caused_by` on lifecycle spans.** An auto (panic) restart *does* have a
  knowable cause — the crashing message's run. Record it as a `caused_by`
  attribute on `node.restart` (plus the `follows_from` link) without adopting it as
  the restart's own correlation — "link/attribute, don't inherit."
