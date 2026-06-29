# RFC: Engine Stress Testing

> **Status: proposed.** A scenario-based stress/fuzz harness for the engine +
> runtime: spin up a real `Engine`, throw randomized work and injected faults at
> it, and assert it never violates a set of invariants — all reproducible from a
> single seed. The scenario harness + invariants is the scope of this effort;
> deterministic concurrency simulation (`loom` / `madsim`) is noted as future
> depth. Tracked in the [roadmap](../reference/roadmap.md#housekeeping).

## Concept

Spin up a real `Engine`, build a random graph, throw a randomized stream of work
and injected faults at it, and assert it never violates a set of invariants — all
reproducible from a `u64` seed.

The point is **not** to diff outputs against a reference: there's no peer engine
that implements fuchsia's contract to diff against, and the engine is legitimately
nondeterministic (at-most-once shedding on a full mailbox, interleaving of
concurrent runs, timing). The point is to **throw chaos at the live system and
check it always behaves** — the way the node-failure-handling review surfaced real
bugs only once we reasoned past the happy path.

## Motivation

[Node failure handling](./node-failure-handling.md) added a lot of concurrent
lifecycle machinery — a per-node supervisor, `catch_unwind`+rebuild,
restart/revival, the dead-letter sink, poison quarantine — exercised today only by
**sequential, single-threaded `#[tokio::test]`s**. That suite is thorough on logic
but:

- never runs under real thread parallelism (every test is current-thread), so the
  cross-thread ordering of the supervisor's atomics + the registry/router locks is
  reasoned-about, not tested;
- doesn't exercise concurrent *operations* racing each other (`push` vs
  `restart_node` vs `remove_graph`);
- has no randomized scenario coverage — every test is a hand-written path.

Two real bugs — an unbounded re-delivery rebuild loop, and a teardown-panic that
zombified a restart node — surfaced from one round of manual scrutiny. That's the
signal: the concurrency/lifecycle corners need a harness that generates the
scenarios we wouldn't think to write by hand.

## Design

A test that, from a seed, generates and runs a random scenario against a live
`Engine` on a **multi-threaded** runtime, then asserts invariants. The seed makes
any failure reproducible.

**A scriptable test actor.** One configurable actor whose per-message behavior is
data: `ok` / `err` / `panic` / `slow(ms)` / `emit(port)`, selected per message (by
index or message tag). This is the fault-injection primitive — every scenario is
built from instances of it plus the builtins (`passthrough`, `if`, `switch`).

**A scenario generator.** From the seed: a random **DAG** of nodes (acyclic, so
`add_edge` accepts it), random edges/ports, per-node random `FailurePolicy`
(continue / fail / retry / route_to_error, restart, poison), a recording
host-provided dead-letter sink, a random stream of `push` /
`push_durable_attempt`, and interleaved lifecycle ops (`restart_node`,
`remove_graph`, further `add_node` / `add_edge`). Messages carry a tag so each
one's fate is traceable.

**Drive to quiescence, then assert.** Run the scenario, wait until every mailbox
is drained and nothing is in flight (a quiescence poll on Health / route
counters), then check:

- **Conservation** — every message pushed has exactly one accounted fate
  (handled / errored / routed-to-error / dead-lettered / poison-quarantined /
  shed / no-route), each of which bumps a counter or lands in the sink. Nothing
  silently vanishes — the system's core promise, made checkable.
- **No zombies** — every node is either resolvable-and-alive or fully
  deregistered; never a registered-but-dead mailbox.
- **Liveness / no deadlock** — every operation, and reaching quiescence, completes
  within a generous timeout.
- **Acyclicity** — no `add_edge` ever created a cycle.
- **Budget accounting** — a restart-enabled node never permanently dies before
  `max_restarts` *node*-attributed crashes; a poison message is quarantined within
  `poison_after`; message-attributed crashes spare the budget.

Run it across many seeds (a loop, or `proptest` for shrinking to a minimal
reproducer).

### Future — deterministic concurrency simulation

For seed-reproducible *concurrency* bugs: `loom` / `shuttle` over the supervisor's
atomic state machine (`parked_dead` / `force` / `restarts`) and the
registry/router lock ordering; `madsim` for whole-engine deterministic simulation
under a seeded runtime with fault injection (the FoundationDB-style approach). Out
of scope for this effort; the scenario harness comes first.

## Alternatives considered

- **Differential against an oracle** (the database-fuzzing approach) — nothing
  implements fuchsia's contract to diff against, and an in-house reference *model*
  (a sequential sim that predicts exact outputs, diffed in a quiescent regime) was
  considered but **set aside**: the invariants above catch the failures we care
  about without the cost of keeping a second implementation in sync. Revisit only
  if a logic bug ever slips past them.
- **Only more hand-written tests** — keeps missing the scenarios we don't think
  of; the two review bugs are the evidence.

## Open questions

- **Where the harness lives** — a `tests/` integration test in `fuchsia-engine`,
  or a small reusable helper crate so the scriptable actor + generator are shared.
- **How aggressively to remove nondeterminism** for the conservation check
  (unbounded mailboxes for an exact count, vs. accounting for legitimate
  at-most-once shedding).
- **`proptest` from the start** (shrinking → minimal reproducers) vs. a hand-rolled
  seeded loop first.
