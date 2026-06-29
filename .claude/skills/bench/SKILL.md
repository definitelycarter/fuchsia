---
name: bench
description: Use when running, adding, or interpreting benchmarks in this repo. Covers the targeted before/after workflow with criterion, noise thresholds, and how to add a new bench. Bench coverage is still ramping up — this skill grows as harnesses are added.
---

# Benchmarks in fuchsia

We benchmark with **criterion**, targeted before/after by default: capture the affected benches on `main`, make the change, capture them again, compare. Always running the full suite is too slow to be a habit.

> **Status (2026-05-23):** initial bench coverage for the actor runtime has landed. Sections 1 and 2 are now populated — keep them current as more harnesses are added.

## 1. What benches exist

| Crate | Bench harness | What it measures |
|-------|---------------|------------------|
| `fuchsia-runtime` | `runtime` | The recv→handle→ack hot path: `runtime/spawn` (instantiate + `setup` an actor), `runtime/deliver` (enqueue one message), `runtime/roundtrip` (full deliver→handle of one message through the tokio task hop). |
| `fuchsia-actor-builtins` | `conditioning` | The native conditioning operators' `handle` in isolation — `builtins/passthrough_handle` (pure forward) and `builtins/dedup_handle` (suppress-duplicate state). Just the actor's per-message work, no engine/runtime. |
| `fuchsia-runtime` | `context` | The per-delivery `ActorContext` build in isolation (`context/per_message`) — the construction the recv→handle loop does for every message: `execution_id` from the correlation, `node_id` shared from the actor's stable id, `task_id` minted fresh. Times only the context build, so the allocation profile is visible (e.g. the `Arc<str>` ids change). |
| `fuchsia-engine` | `routing` | The routing hot path in isolation — `RoutedEmit::emit` → `RouterState::route` plus the per-port counter bump, via the `Engine::emit_sink` bench seam (no actor task / recv loop). `route_single`; `route_fanout/{1,4,16}` (per-port fan-out); `route_concurrent/{2,4,8}` (N threads emitting in parallel — the contention shape that motivated the lock-free counter design). |

Run a single harness once they exist:

```bash
cargo bench -p <crate> --bench <name>
```

## 2. Affected-bench map

Use this to pick which benches to run for a given change. **Widen when uncertain** — running an extra bench is cheaper than missing a regression.

| Source area | Run these benches |
|-------------|-------------------|
| `crates/fuchsia-actor/src/actor.rs` (trait shape, `async-trait` boxing) | `fuchsia-runtime::runtime` (the recv→handle loop) |
| `crates/fuchsia-actor/src/actor.rs` (`ActorContext` shape / id types) + `crates/fuchsia-runtime/src/runtime.rs` (`run_actor` per-message context build) | `fuchsia-runtime::context`, `fuchsia-runtime::runtime` (the `runtime/roundtrip` group) |
| `crates/fuchsia-runtime/src/registry.rs` (instantiate path) | `fuchsia-runtime::runtime` (the `runtime/spawn` group) |
| `crates/fuchsia-engine/src/router.rs` (routing table, route, counters) | `fuchsia-engine::routing` |
| `crates/fuchsia-actor-builtins/src/{passthrough,dedup,…}.rs` (conditioning operators' `handle`) | `fuchsia-actor-builtins::conditioning` |

Not yet covered (consider adding harnesses when these areas change materially):

- `crates/fuchsia-actor-wasm/` — wasm component instantiation, per-message Store creation, host import dispatch
- `crates/fuchsia-actor-lua/` — Lua VM creation + script load per message
- `crates/fuchsia-capabilities/` — HTTP client overhead (depends on real network, harder to bench reliably)

When you add a bench, add a row above mapping the source area to the harness name.

## 3. The before/after workflow

Use criterion's `--save-baseline` so you don't have to keep the `main` checkout around.

```bash
# On main (or a clean base), before the change
git switch main
cargo bench -p <crate> --bench <name> -- --save-baseline before

# Switch to your branch, make the change, then:
cargo bench -p <crate> --bench <name> -- --baseline before
```

Criterion prints a colored diff per benchmark group. The number that matters is the **change in mean time** with the confidence interval.

Capture the diff output (paste into the commit body, the PR description, or a scratch file) so it's preserved beyond the terminal session — criterion overwrites baselines on the next run.

## 4. Reading the numbers

- **< 5% change**: noise. Re-run once before believing it. Criterion's own variance is 1-3%.
- **5-10%**: real, but only worth surfacing if the bench is on a documented hot path.
- **> 10%**: real and material. Mention in the commit body. If regressing, surface to the user before committing — do not commit a > 10% regression without explicit acknowledgement.
- **Confidence interval crosses zero**: not significant, regardless of mean.

When a number is material (regression *or* improvement > ~10% on a documented bench), include it in the commit body:

```
orchestrator dispatch 1k nodes: 142ms → 98ms (-31%)
```

Don't pad commit bodies with bench numbers when nothing meaningfully moved.

## 5. Adding a new benchmark

First bench in a crate? Add the dev-dependency. Fuchsia's workspace doesn't use `[workspace.dependencies]`, so add it per-crate:

```toml
# crates/<crate>/Cargo.toml

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "your_bench"
harness = false
```

Then create `crates/<crate>/benches/your_bench.rs`:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_something(c: &mut Criterion) {
    c.bench_function("operation_name", |b| {
        b.iter(|| {
            // the operation under test
            black_box(do_work(black_box(input)))
        });
    });
}

criterion_group!(benches, bench_something);
criterion_main!(benches);
```

Then:

1. Run it once on `main` and save the baseline (`-- --save-baseline main`) so future before/after runs have something to compare to.
2. Add a row to the "What benches exist" table above.
3. Add a row to the affected-bench map keyed to the source area it covers.

A new bench should measure one specific operation. If you find yourself wanting to add five `bench_*` functions to one file, split them into separate harnesses — easier to interpret diffs and easier to skip irrelevant ones.

Honour `AGENTS.md` in bench code: avoid `unwrap()` / `expect()` / `.ok()` / `clone()` outside of the setup phase. Setup code that runs once before timing is held to a looser bar (it's not in the measured loop), but the body of `b.iter(|| ...)` should be clean — a panic there will skew or corrupt the run.

## 6. Periodic full sweep

Run the full suite:

- After a large refactor that touched cross-cutting code (orchestrator scheduling, runtime trait, host capability surface).
- Before tagging a release.
- Any time the affected-bench map felt ambiguous and you want a sanity check.

```bash
cargo bench
```

This is slow (multiple minutes once benches accumulate). Don't put it in a pre-commit hook.

## 7. Failure modes to avoid

- Running `cargo bench` (everything) when only one harness is affected — wastes minutes per commit.
- Trusting a single sub-5% delta — re-run before believing it.
- Forgetting to save the `main` baseline before switching branches, then having to checkout `main`, re-bench, and switch back.
- Including bench numbers in commit bodies when they didn't materially move (noise).
- Adding a bench without registering it in section 1 and the affected-bench map in section 2 — the map decays the moment it stops being maintained.
- Forgetting `harness = false` in `Cargo.toml` — criterion uses its own harness and the default `libtest` harness will fight it.
