---
name: docs
description: Use at the end of a feature or refactor before committing. Identifies which docs are affected by the change (mdBook pages, roadmap, README, AGENTS.md, per-crate READMEs), updates them in the same commit, and keeps the roadmap honest about what's done and what's planned.
---

# Updating docs in fuchsia

The rule: **docs change in the same commit as the code that made them stale.** Doc drift compounds, and the commit log has precedent for this: `2ceed3f` ("Updated: AGENTS.md, DESIGN.md, TODO.md" bundled with the runtime restructure), `dfbacf4` (mdBook site added alongside the runtime unification), `38b2afd` (AGENTS.md updated with the WIT cleanup), `a166de8` (README + AGENTS bundled).

This skill is for end-of-feature doc updates. It is not for writing new standalone documents — see "What not to write" at the bottom.

## 0. Doc surfaces in this repo (read this first)

- `README.md` — overview, feature list
- `AGENTS.md` — coding guidelines, project structure, dev commands. **Frequently drifts** (currently lists removed pre-unification crates) — touch this whenever crate layout or workspace conventions change.
- `docs/book/src/` — published mdBook. Canonical for architecture, runtimes, workflows, components, data model, examples, reference.
- `docs/DESIGN.md`, `docs/USE_CASES.md`, `docs/ANALYSIS.md`, `docs/COMMAND_EXECUTION.md` — **legacy top-level docs that duplicate mdBook pages.** They predate the mdBook migration. Treat the mdBook as canonical; if you touch a topic that exists in both, update both in the same commit and consider flagging the duplication to the user so it can be resolved at the source.
- `crates/<crate>/README.md` — per-crate readmes (currently `fuchsia-component-registry`, `fuchsia-resolver`).
- `examples/README.md` — examples index.

## 1. Affected-doc map

Pick the docs to touch based on what the change altered. Widen when uncertain.

| Code area / change type                                                | Update these docs                                                                                                                     |
|------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| Anything that lands a roadmap item (Features or Gaps row)              | `docs/book/src/reference/roadmap.md` (remove the row — see step 3)                                                                    |
| Anything that adds new planned work, gap, or open question             | `docs/book/src/reference/roadmap.md` (add row to relevant table)                                                                      |
| New crate, removed crate, workspace member change                      | `docs/book/src/reference/crate-map.md`, **`AGENTS.md`** (Project Structure section), root `Cargo.toml` workspace members              |
| New trait, crate boundary change, or runtime architecture shift        | `docs/book/src/architecture/overview.md` plus the relevant subpage (`engine.md`, `runtimes.md`, `host-capabilities.md`, `component-registry.md`); often `docs/DESIGN.md` mirrors |
| Orchestrator scheduling / graph traversal / parallel execution         | `docs/book/src/architecture/engine.md`, `docs/book/src/workflows/execution.md`                                                        |
| Runtime executor changes (wasm / lua / js)                             | `docs/book/src/runtimes/<wasm\|lua\|js>.md`, `docs/book/src/architecture/runtimes.md`                                                 |
| Host capability changes (kv / config / log / http)                     | `docs/book/src/architecture/host-capabilities.md`, `docs/book/src/components/capabilities.md`                                          |
| WIT interface changes (`wit/deps/<task\|trigger\|kv\|config\|log>`)    | `docs/book/src/components/<tasks\|triggers\|capabilities>.md`; the `.wit` files themselves are documentation — keep comments current  |
| Component registry / manifest / packaging changes                      | `docs/book/src/architecture/component-registry.md`, `docs/book/src/components/packaging.md`, `crates/fuchsia-component-registry/README.md` |
| Workflow config schema or resolver changes                             | `docs/book/src/workflows/config.md`, `docs/book/src/workflows/resolution.md`, `crates/fuchsia-resolver/README.md`                     |
| Trigger handling (poll / webhook / WASI incoming-request)              | `docs/book/src/workflows/triggers.md`, `docs/book/src/components/triggers.md`                                                         |
| Input resolution (minijinja, type coercion)                            | `docs/book/src/workflows/input-resolution.md`                                                                                          |
| Node data / error type changes                                         | `docs/book/src/data-model/<node-data\|errors>.md`                                                                                      |
| New or changed example workflow                                        | `docs/book/src/examples/use-cases.md`, `docs/USE_CASES.md` (legacy mirror), `examples/README.md`                                       |
| New benchmark, or bench numbers moved materially                       | Run the [[bench]] skill; benchmarks aren't documented in the book yet — when a `docs/book/src/reference/benchmarks.md` lands, add a row here |
| User-visible API, CLI command, or feature                              | `README.md` (overview / feature list)                                                                                                  |
| Build / test / contribution conventions change                         | `AGENTS.md` (Development and Guidelines sections)                                                                                      |

If a change touches multiple rows, take the union.

## 2. The check

Before drafting the commit, scan each doc above that *might* be affected for stale lines: code samples that won't compile, names that were renamed, claims about behavior that changed, missing entries for new features. The skill is doing this scan, not relying on memory.

```bash
# Renamed symbol / removed crate / changed function name
rg '<old-name>' docs/ README.md AGENTS.md crates/*/README.md
```

If you can't find drift but the change is substantial (new feature, new crate, behavior change), the answer is almost never "no docs need updating" — look again, especially at `AGENTS.md` (drifts silently) and `docs/book/src/reference/crate-map.md`.

## 3. Roadmap hygiene

The roadmap (`docs/book/src/reference/roadmap.md`) is structured as tables — `Features`, per-crate `Gaps`, `Open Questions`, `Housekeeping` — not as narrative sections with design notes (this is different from typical roadmap formats).

When a roadmap item lands:

- **Remove the row** from its table. The table format doesn't carry a "done" state, and a stale row is worse than a missing one.
- **Capture follow-ups discovered during implementation** as new rows in the appropriate table (a new Gap, a new Open Question) — better to write down than to forget. Don't drop loose ends on the floor.
- **Don't leave the row with strikethrough or a `~~done~~` marker** — there's no precedent for that here and it makes the table noisy.

When adding new planned work, match the existing table shape: `| Feature | Description | Notes |` for the top-level Features table; `| Gap | Priority |` for per-crate Gaps; `| Question | Context |` for Open Questions. Keep descriptions one line — the table format enforces brevity, and that's a feature.

## 4. README and AGENTS.md drift

These two are the most likely to silently drift, because nothing forces them to update:

- **`README.md`** quotes the feature list. Any user-visible capability added or removed needs to land here in the same commit.
- **`AGENTS.md`** currently lists pre-runtime-unification crates that no longer exist (`fuchsia-runtime`, `fuchsia-engine`, `fuchsia-host`, `fuchsia-task-host`, `fuchsia-trigger-host`, `fuchsia-task`, `fuchsia-trigger`). If you touch crate layout, fix the Project Structure section in the same commit — don't perpetuate the drift.

## 5. Commit-time integration

Doc updates go in the **same commit** as the code change that motivated them. Two commits ("code change" then "update docs") is the wrong pattern — it leaves `main` in a stale-doc state at the intermediate commit, and the docs commit ends up without context.

The commit message body should mention doc updates explicitly when they're substantive. Precedent: `2ceed3f` closes its body with `Updated: AGENTS.md, DESIGN.md, TODO.md`. Either pattern works:

```
Updated: AGENTS.md, docs/book/src/architecture/engine.md
```

or in prose: "Also updates the architecture overview to reflect the new orchestrator boundary."

See the [[commit]] skill for the rest of the commit workflow.

## 6. What not to write

`AGENTS.md` is implicit on this and CLAUDE.md instructions are explicit: **don't create unrequested `.md` files**. Specifically:

- Don't write planning docs, decision logs, "thoughts" files, or `NOTES.md` unless the user asks.
- Don't create per-feature design docs as separate files — design notes belong inside the relevant mdBook page (typically under `docs/book/src/architecture/` or `docs/book/src/workflows/`).
- Don't write tutorial content speculatively — wait until the user signals it's wanted.
- **Don't add more top-level `docs/*.md` files** that duplicate mdBook content — the existing duplication (`docs/DESIGN.md` ↔ `docs/book/src/architecture/*`, etc.) is already a maintenance hazard; don't make it worse.

If you find yourself wanting to create a new `.md` file, the answer is almost always "add a section to an existing mdBook page" instead.

## 7. Failure modes to avoid

- Committing code that adds a feature without updating `README.md` or the architecture pages in `docs/book/src/architecture/`.
- Updating crate layout without touching `AGENTS.md` Project Structure or `docs/book/src/reference/crate-map.md` — the most common silent-drift pattern in this repo.
- Marking a roadmap row done by adding a "~~done~~" or "DONE" marker instead of removing it.
- Updating docs in a follow-up commit (leaves an intermediate stale state).
- Writing speculative planning files outside `docs/book/src/reference/roadmap.md`.
- Updating `docs/book/src/.../page.md` but not its `docs/*.md` legacy mirror (or vice versa), leaving the duplicates more divergent than before.
- Editing the mdBook `SUMMARY.md` to add a page and forgetting to create the page itself (broken link in published output).
