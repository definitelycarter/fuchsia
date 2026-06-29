# Roadmap

Outstanding features, known gaps, and open questions. Rows are removed when work
lands (no strikethrough).

## Features

| Feature | Description | Notes |
|---------|-------------|-------|
| Graceful shutdown | `engine.shutdown(deadline)` — seal entrypoints, drain source → sink, run each `teardown`, deadline-bounded; requires a DAG. See [RFC](../rfcs/graceful-shutdown.md). | `fuchsia-engine`, `fuchsia-runtime` |
| Runs & result correlation | Persistent graph; runs are correlation-tagged fire-and-forget messages; optional async result via a respond node + result sink. See [RFC](../rfcs/runs-and-results.md). | `fuchsia-engine`, host |
| JavaScript actor (QuickJS) | Dynamic JS scripts in an embedded QuickJS interpreter (`rquickjs`, no compile), mirroring the Lua pack; `await fetch()` via an injected async capability. Compile-to-wasm is the hardened alternative. See [RFC](../rfcs/javascript-actor.md). | `fuchsia-actor-js` (new), `fuchsia-actor` |
| More conditioning operators | Throttle, window, threshold-over-time to round out the existing `debounce`/`deadband`/`dedup` set | `fuchsia-actor-builtins` |
| Config import for guests | Forward a `Component` node's `settings` into a Wasm/Lua guest (e.g. a `config.get(key)` import). Today only native actors read `settings`; guests receive only `ctx` + payload. | `fuchsia-actor-wasm`, `fuchsia-actor-lua` |
| Capability-style device binding | Bind each actor instance to one host-side device handle (BLE/MQTT/…) so guest-side functions never name addresses | host crates, per-capability WIT |
| Distributed actors | Patterns + sample host code for splitting a graph across processes via transport actors | likely host docs, not core |

## Gaps

### `fuchsia-transport` / `fuchsia-runtime`

| Gap | Priority |
|-----|----------|
| Mailbox capacity is a hardcoded `mailbox(32)` in `spawn_with_caps`; not configurable per-node or per-graph | Medium |
| A long-running `handle` runs to completion — there is no mid-call interruption (cancellation is between messages, via mailbox close) | Medium |
| Uncounted at-most-once losses (surfaced by [engine stress testing](../rfcs/engine-stress-testing.md)): a transient restart (a caught panic that rebuilds under budget) bumps no `Health` counter, so a flapping node is unobservable and its dropped in-flight message is uncounted — a crash counter closes it (fix in progress); and a `push` shed at the entrypoint is uncounted (its doc overclaims). | Medium |

### `fuchsia-engine`

| Gap | Priority |
|-----|----------|
| `add_edge` enforces acyclicity with a full reachability walk per edge (O(V+E)) — fine at workflow scale. If graphs ever grow large, switch to incremental topological maintenance (keep a topo order, check on insert). See [DAG enforcement](../rfcs/dag-enforcement.md). | Low |
| Routing sheds on a full downstream mailbox (at-most-once) with no per-target backpressure option | Low (intentional for the conditioning path; revisit if a lossless route is needed) |
| `Emit::emit_to` returns `()`, so a caller can't tell delivered from routed-nowhere. Returning a routing outcome (delivered / shed / no-route) would let the runtime fall a `route_to_error` emit through to the dead-letter sink when the `"error"` port is unwired — the precedence [node failure handling](../rfcs/node-failure-handling.md) defines but can't yet realize. | Low |
| `Engine::restart_node` can't revive a `fail`-stopped restart-enabled node: a `fail` stop exits the supervisor, but the engine keeps the node's restart handle until `remove_graph`, so `restart_node` reports it live (`AlreadyRunning` / a no-op `force`) instead of reviving. Needs the runtime to distinguish a *fail-death* (drop the handle) from a *budget-death* (keep it, parked + revivable). No leak — the supervisor exits; only the small handle lingers. See [node failure handling](../rfcs/node-failure-handling.md). | Low |

### `fuchsia-actor-wasm`

| Gap | Priority |
|-----|----------|
| No epoch/fuel ticker wired by default — a custom `Config` can enable it, but `WasmActorCreator::new` builds a plain engine, so hard deadlines on in-flight calls don't fire out of the box | Medium |

## Open Questions

| Question | Context |
|----------|---------|
| Machine-readable schema for actor configs | Each actor dictates its own `settings` type; no schema for tooling/plugin-store UI. Could derive via `schemars`. |
| Replay / in-flight inspection | Should the runtime support observing messages in mailboxes for debugging? |
| Routing counters' surface | The engine now tracks per-`(node, port)` `delivered`/`shed`/`no_route` counts in-process ([named output ports](../rfcs/output-ports.md)); whether they graduate to a metrics/trace export is a later observability decision. |

## Housekeeping

| Item | Priority |
|------|----------|
| The `fuchsia-actor-wasm` integration test and `fuchsia-examples` need `test-components/actor-echo` built first (they skip / print instructions otherwise); a CI step should build it so the wasm path is actually exercised | Medium |
| Engine/runtime stress testing — a seeded scenario harness (throw randomized work + faults at a live engine on a multi-threaded runtime; assert conservation / no-zombies / liveness / budget invariants). The lifecycle machinery from [node failure handling](../rfcs/node-failure-handling.md) is covered only by sequential single-threaded tests today. See [RFC](../rfcs/engine-stress-testing.md). | High |
