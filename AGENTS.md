# Fuchsia

Fuchsia is an actor-based dataflow runtime. A workflow is a graph of actors;
routing between nodes lives in the graph (configuration), not in actor code.
Each actor is a native Rust implementation, a Wasm component, or a Lua script,
all behind a single `Actor` trait (`setup`/`handle`/`teardown`, async,
over `&mut self`).

It is **handle-per-message**: the runtime owns the receive loop and calls the
actor's `handle` per message — actors do not run their own loop. An actor's
powers beyond receive-and-emit are a typed capability bag injected at
construction: fuchsia ships only `emit` (the engine) and `schedule` (the
runtime); any domain capability (a state sink, http, …) is inserted by the
product under its own trait type. Fuchsia owns one guest contract —
`fuchsia:actor` (lifecycle + emit +
payload types) — and does not prescribe what else a Wasm/Lua actor may import;
products add their own capability imports through the `WasmHost` / `LuaHost`
seam.

## Project Structure

Crates are layered bottom-up; each depends only on the layer below.

- `crates/`
  - `fuchsia-actor` — The contract: `Actor` trait, `ActorCreator`/`ActorFactory`,
    `ActorCapabilities` (`Emit` / `Schedule`, plus the open bag for host capabilities), `Message` /
    `MessageValue`, `ActorContext`, `ActorConfig` (env + bson settings),
    `ActorId`, `ActorError`, `COMPONENT_ENV_KEY`. The lean surface actor packs
    depend on.
  - `fuchsia-transport` — Delivery plumbing: bounded `mailbox` (mpsc of
    `Delivery`), `Delivery` (message + `Ack` + trace span), `Ack`
    (`Health` at-most-once / `Complete` at-least-once), `Offer`. No `Transport`
    trait — durability is layered in front of the channel.
  - `fuchsia-runtime` — The actor substrate: `Runtime` owns the
    recv→handle→ack loop (one tokio task per actor), runs the lifecycle, and
    provides the `schedule` capability (`TokioSchedule`). `ActorRegistry` is
    the live `ActorHandle` address book. Criterion bench under `benches/`.
  - `fuchsia-engine` — Routing per a graph's edges. `Engine` (shareable as
    `Arc`) does `add_node` / `add_edge` / `remove_graph` / `push` over a live
    `RouterState`, and provides the `emit` capability (`RoutedEmit`). `add_edge`
    rejects cycle-creating edges (self-loops and back-edges), so graphs stay
    acyclic. Knows only actors + addressing.
  - `fuchsia-actor-builtins` — Native builtin actors: `passthrough`,
    `debounce`, `deadband`, `dedup`, the branching nodes `if` / `switch` (over a
    `Condition` enum — declarative `field`/`op`/`value` with `all`/`any`, plus a
    minijinja `expr` arm), plus `register`.
  - `fuchsia-actor-wasm` — Wasm-component-hosting actors. `WasmActor<H: WasmHost>`
    + `WasmActorCreator<H>` (one creator per `"wasm"` runtime, component
    catalog; component id from `ActorConfig.env`) + `BaseHost` (contract-only:
    links `emit`, traps other imports). Async wasmtime (`exports: async`,
    `call_async`) drives the lifecycle, while the guest's WIT calls stay
    synchronous.
  - `fuchsia-actor-lua` — Lua-script-hosting actors. `LuaActor<H: LuaHost>` +
    `LuaActorCreator<H>` (one creator per `"lua"` runtime, script catalog) +
    `BaseLuaHost` (registers only `emit`). Async mlua (`call_async`) drives the
    lifecycle, while the guest's Lua scripts stay synchronous; `mlua` uses the
    `vendored` feature (statically links its own native `lua` lib).
  - `fuchsia-examples` — Runnable demo wiring a Lua actor, a builtin, and a
    Wasm component into one engine graph (`cargo run -p fuchsia-examples`).
- `wit/` — Ships **only** the `fuchsia:actor` package; no bundled platform
  world (those belong to products), no http/log/wasi interfaces.
  - `actor.wit` — Lifecycle: `setup(ctx)`, `handle(ctx, msg)`, `teardown(ctx)`,
    all returning `result<_, string>`.
  - `types.wit` — `payload` (`%type` + `payload-value`: json | binary | empty).
  - `emit.wit` — Host-imported `emit.send-to(port, payload)` (per named output
    port) and `emit.send(payload)` (the `"out"` default).
- `test-components/actor-echo/` — Standalone-workspace crate that compiles to a
  wasm component (imports only `fuchsia:actor`) for the `fuchsia-actor-wasm`
  integration test. Requires `cargo component build --release`; the test and the
  example skip / print instructions if the artifact is absent.
- `docs/book/` — Published mdBook (architecture, actor implementations,
  reference, RFCs). Canonical design documentation.
- `.claude/skills/` — Per-skill instructions (commit, bench, docs, rfc, worktree).

## Development

```bash
cargo build --workspace
cargo test --workspace
cargo fmt
```

Benches (criterion): `cargo bench -p fuchsia-runtime --bench <name>` —
see `.claude/skills/bench/SKILL.md` for the targeted before/after workflow.

## RFCs & worktrees

Substantial designs are captured as RFCs before or alongside the code, then
implemented in an isolated worktree — the cadence used in the sibling `slate` repo.

- **RFCs** live in `docs/book/src/rfcs/<slug>.md`, registered in
  `docs/book/src/SUMMARY.md` under `# RFCs` and listed on the RFC index page. Each
  opens with a one-line `> **Status:** …` callout and links to the roadmap. See the
  `rfc` skill.
- **Worktrees** for implementing an RFC live under `.claude/worktrees/<slug>/`
  (gitignored) — an isolated checkout on its own branch so `main` stays clean and
  efforts can run in parallel. See the `worktree` skill.

## Guidelines

- Follow Rust idioms and best practices
- Use `cargo fmt` before committing
- Ensure all tests pass with `cargo test --workspace`
- Add tests for new functionality
- Do not automatically commit or push to this repository — wait for explicit user approval
- Avoid `clone()` in production code — provide justification if proposing it (acceptable in tests; refcount-bumping clones of `Arc` / `mpsc::Sender` / `CancellationToken` / `Engine` / `Component` are accepted with brief justification)
- Avoid `unwrap()`, `expect()`, and other panic-prone error handling in production code (acceptable in tests and bench setup; iter-body bench panics are acceptable as invariant assertions)
- Avoid `.ok()` to silently discard errors in production code — propagate errors with `?` or `map_err` instead (acceptable in tests and in `sort_by` closures where returning `Result` is not possible)
