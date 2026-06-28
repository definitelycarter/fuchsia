---
name: rfc
description: Use when proposing, writing, or updating a design RFC in this repo. Covers where RFCs live in the mdBook, the status lifecycle and the status callout, the structure/template, registering in SUMMARY, how an RFC links to the table-based roadmap, and how it graduates into a worktree for implementation.
---

# RFCs in fuchsia

Substantial designs get written up as an RFC **before or alongside** the code, so
the *why* is captured where the design lives — in the published mdBook — not in a
commit message or a scratch file. This is the cadence used in the sibling `slate`
repo, adapted to fuchsia's layout.

Write an RFC when a change is bigger than a single obvious edit: a new crate or
trait, a capability seam, a routing/transport semantics decision, a guest-contract
change, anything with an open question or a trade-off worth recording. Skip it for
mechanical refactors, bug fixes, and doc-only changes — those just go straight to a
commit (see [[commit]]).

## Where RFCs live

- One file per RFC: `docs/book/src/rfcs/<slug>.md`. The slug is kebab-case and
  matches the topic (`per-actor-retry-policy.md`, `config-import-for-guests.md`).
- Registered in `docs/book/src/SUMMARY.md` under the `# RFCs` section, listed
  below the index page. **Every SUMMARY link must point at a file that exists** —
  add the page and the SUMMARY line together, or `mdbook build` breaks the
  published output.
- The process and template are documented on the index page
  (`docs/book/src/rfcs/index.md`) — read it before writing your first one.

Do **not** put RFCs in top-level `docs/*.md`, in `scratch/`, or in a per-crate
file. The book under `docs/book/src/` is the single home for design documentation
(the [[docs]] skill is strict about this).

## The status callout

Every RFC opens with a one-line blockquote callout, immediately under the title,
that states where it is in its life and links to the roadmap:

```markdown
# RFC: Per-Actor Retry Policy

> **Status: proposed.** Tracked in the [roadmap](../reference/roadmap.md#features)
> Features table until it lands.
```

The callout is the single source of truth for an RFC's status. Keep it current —
when work moves, edit this line in the same commit as the code that moved it.

### Lifecycle states

Flow top to bottom; an RFC can also branch to a terminal off-ramp.

| State | Meaning |
|-------|---------|
| `proposed` | Written up, not yet agreed. The default for a new RFC. |
| `design spike` | A recommendation/survey only — no engine code is committed by this RFC. |
| `accepted` | Agreed; ready to implement. May not be started. |
| `in progress` | Implementation underway; note which parts have landed (e.g. *steps 1–2 shipped, 3–4 pending*). |
| `partially implemented` | Some of the design shipped; the rest is deferred, with the gap named. |
| `implemented` / `shipped` | Fully landed. The RFC stays in the book as the durable record of the decision. |
| `superseded by [[other-rfc]]` | Replaced by a newer design. Leave the file; point at the successor. |
| `rejected` | Decided against. Keep it — a recorded "no" is worth as much as a "yes". |

Be specific in the callout when status is partial — name what shipped and what's
left (`> **Status: in progress — schedule wiring done, backoff policy pending.**`).
Slate's RFCs do this and it's the most useful part of the line.

## Structure

There's no rigid section schema — match the weight of the decision. A small design
spike is a few hundred words; a load-bearing one runs long. A good default:

```markdown
# RFC: <Title>

> **Status: proposed.** <one line + roadmap link>

## Concept
What this is, in a paragraph. The shape of the thing.

## Motivation
The problem today. Why the status quo is insufficient. What this unblocks.

## Design
The proposal. For fuchsia, be explicit about which layer owns what — name the
crate(s): `fuchsia-actor` (contract) / `fuchsia-transport` / `fuchsia-runtime` /
`fuchsia-engine` / `fuchsia-actor-{builtins,wasm,lua}`. Capabilities arrive as a
typed bag at construction — say whether this adds one, and on which seam
(`WasmHost` / `LuaHost` for guests).

## Alternatives considered
The other paths and why they lost. Include "status quo (do nothing)".

## Open questions
What's unresolved. Better written down than forgotten.
```

Add or drop sections freely (`Migration`, `Performance note`, `Follow-ups`,
`Test plan`). Keep code samples compiling against the current API — a stale sample
in an RFC reads as a bug.

## Roadmap linkage

Fuchsia's roadmap (`docs/book/src/reference/roadmap.md`) is **table-based**, not
narrative — so the linkage differs from slate:

- When you write an RFC for planned work, link it from the relevant roadmap row's
  description/notes (the **Features** table for a new feature, a per-crate **Gaps**
  row for a gap). The row points at the RFC; the RFC's callout carries the detail.
- The roadmap tables have no "done" column. When the work **fully lands**, *remove
  the row* (per the [[docs]] roadmap-hygiene rule) and flip the RFC callout to
  `implemented`. The RFC — not a strikethrough row — is the durable record.
- Follow-ups discovered while implementing become new Gaps/Open-Questions rows, or
  an `## Open questions` entry in the RFC. Don't drop loose ends.

## Graduating to implementation

An accepted RFC is implemented in an isolated **git worktree**, not on `main`. The
branch and worktree are named after the RFC slug so the link is obvious. See the
[[worktree]] skill for the mechanics. As the implementation lands, update the RFC
callout (`proposed` → `in progress` → `implemented`) and the roadmap together with
the code — never in a trailing "update docs" commit.

## Checklist

- [ ] `docs/book/src/rfcs/<slug>.md` created, opens with a `> **Status:** …` callout.
- [ ] Listed in `docs/book/src/SUMMARY.md` under `# RFCs`; `mdbook build` is clean.
- [ ] Linked from the roadmap if it represents planned work.
- [ ] Layer ownership named in the Design section (which crate, which seam).
- [ ] Callout and roadmap updated in the same commit whenever status moves.
