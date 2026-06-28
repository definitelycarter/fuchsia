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

_None yet — this section lists RFCs as they're written._
