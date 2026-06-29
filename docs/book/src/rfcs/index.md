# RFCs

Substantial designs in fuchsia are written up as an RFC before or alongside the
code, so the reasoning lives where the design does — in this book — rather than in
a commit message. Each RFC is one page under `rfcs/`, listed in
[SUMMARY](../SUMMARY.md) below this index.

Write an RFC when a change carries a real decision: a new crate or trait, a
capability seam, routing/transport semantics, a guest-contract change, or anything
with a trade-off worth recording. Mechanical refactors and bug fixes skip straight
to a commit.

## How to read one

Every RFC opens with a one-line status callout linking back to the
[roadmap](../reference/roadmap.md):

> **Status: proposed.** Tracked in the roadmap Features table until it lands.

The status word places the RFC in its lifecycle:

| State | Meaning |
|-------|---------|
| `proposed` | Written up, not yet agreed. |
| `design spike` | Recommendation/survey only — no engine code committed by this RFC. |
| `accepted` | Agreed; ready to implement. |
| `in progress` | Underway — the callout names what has landed. |
| `partially implemented` | Part shipped; the deferred remainder is named. |
| `implemented` / `shipped` | Fully landed; the RFC remains as the durable record. |
| `superseded` / `rejected` | Replaced or decided against — kept for the record. |

A planned RFC is linked from the matching [roadmap](../reference/roadmap.md) row;
when the work fully lands the row is removed and the RFC callout flips to
`implemented`. Implementation happens in a git worktree under
`.claude/worktrees/<slug>/`, not on `main`.

## Template

```markdown
# RFC: <Title>

> **Status: proposed.** <one line + roadmap link>

## Concept
What this is, in a paragraph.

## Motivation
The problem today; why the status quo is insufficient; what this unblocks.

## Design
The proposal. Name the layer that owns each part — `fuchsia-actor` (contract) /
`fuchsia-transport` / `fuchsia-runtime` / `fuchsia-engine` /
`fuchsia-actor-{builtins,wasm,lua}` — and, for guest capabilities, the seam
(`WasmHost` / `LuaHost`).

## Alternatives considered
Other paths and why they lost, including "do nothing".

## Open questions
What's unresolved.
```

Sections are a default, not a schema — add or drop to match the weight of the
decision.

## Active RFCs

A connected set aimed at evolving the runtime into a workflow/automation backbone.
The first two are foundational — the rest build on them.

| RFC | Status | Summary |
|-----|--------|---------|
| [Named Output Ports](./output-ports.md) | implemented | Multiple named outputs per actor so routing can branch (IF/Switch, error branches) instead of cloning to every successor. |
| [Per-Message Correlation Id](./message-correlation-id.md) | proposed | A run id minted at the trigger and propagated through every hop and the guest boundary, for error and result correlation. |
| [Async Actor Contract](./async-actor-contract.md) | proposed | `Actor` lifecycle goes `async` so handles can `.await` I/O without blocking a thread; the guest WIT stays synchronous (wasmtime drives guests async). Foundational. |
| [Node Failure Handling](./node-failure-handling.md) | proposed | Death detection (the zombie-actor fix), per-node error policy, an error output port, retry, and a dead-letter sink. |
| [Graceful Shutdown](./graceful-shutdown.md) | proposed | Drain-then-teardown: seal entrypoints, drain in dependency order (source → sink), run each `teardown`, bounded by a deadline, returns the force-stopped nodes. Requires a DAG. |
| [DAG Enforcement](./dag-enforcement.md) | proposed | `add_edge` rejects cycle-creating edges — fuchsia graphs are acyclic (what lets graceful-shutdown's topological drain terminate). |
| [Runs & Result Correlation](./runs-and-results.md) | proposed | Persistent graph; runs are correlation-tagged, fire-and-forget messages; optional async result via a respond node. |
| [JavaScript Actor (QuickJS)](./javascript-actor.md) | proposed | Dynamic JS scripts in an embedded QuickJS interpreter (no compile), mirroring the Lua pack; `await fetch()` via an injected async capability. Compile-to-wasm stays as the hardened alternative. |
| [`from_fn` Actors & Default Lifecycle](./from-fn-actors.md) | proposed | Default no-op `setup`/`teardown` so an actor is just a `handle`, plus `from_fn`/`register_fn` to write a node as a closure — no struct, impl, or creator. |
