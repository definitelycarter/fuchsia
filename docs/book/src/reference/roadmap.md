# Roadmap

Outstanding features, known gaps, and open questions. Rows are removed when work
lands (no strikethrough).

## Features

| Feature | Description | Notes |
|---------|-------------|-------|
| Per-message correlation id | A run id minted at the trigger and propagated through every emit/hop and the guest boundary, for error and result correlation. See [RFC](../rfcs/message-correlation-id.md). | `fuchsia-actor`, `fuchsia-transport`, `fuchsia-runtime`, `fuchsia-engine` |
| Node failure handling | Death detection (the zombie-actor fix), per-node error policy, error output port, retry, dead-letter sink. See [RFC](../rfcs/node-failure-handling.md). | `fuchsia-runtime`, `fuchsia-engine`, `fuchsia-actor` |
| Graceful shutdown | `engine.shutdown(deadline)` — seal entrypoints, drain source → sink, run each `teardown`, deadline-bounded; requires a DAG. See [RFC](../rfcs/graceful-shutdown.md). | `fuchsia-engine`, `fuchsia-runtime` |
| Runs & result correlation | Persistent graph; runs are correlation-tagged fire-and-forget messages; optional async result via a respond node + result sink. See [RFC](../rfcs/runs-and-results.md). | `fuchsia-engine`, host |
| JavaScript actor (QuickJS) | Dynamic JS scripts in an embedded QuickJS interpreter (`rquickjs`, no compile), mirroring the Lua pack; `await fetch()` via an injected async capability. Compile-to-wasm is the hardened alternative. See [RFC](../rfcs/javascript-actor.md). | `fuchsia-actor-js` (new), `fuchsia-actor` |
| Per-actor retry policy | Configurable retries with backoff around a node's `handle`, beyond the at-least-once feeder's retry-on-loss. Folded into [node failure handling](../rfcs/node-failure-handling.md). | `fuchsia-runtime` / `fuchsia-engine` |
| More conditioning operators | Throttle, window, threshold-over-time to round out the existing `debounce`/`deadband`/`dedup` set | `fuchsia-actor-builtins` |
| Config import for guests | Forward a `Component` node's `settings` into a Wasm/Lua guest (e.g. a `config.get(key)` import). Today only native actors read `settings`; guests receive only `ctx` + payload. | `fuchsia-actor-wasm`, `fuchsia-actor-lua` |
| Capability-style device binding | Bind each actor instance to one host-side device handle (BLE/MQTT/…) so guest-side functions never name addresses | host crates, per-capability WIT |
| Distributed actors | Patterns + sample host code for splitting a graph across processes via transport actors | likely host docs, not core |

## Gaps

### `fuchsia-transport` / `fuchsia-runtime`

| Gap | Priority |
|-----|----------|
| A panicking `handle` silently zombifies the actor: the task dies, but its `JoinHandle` is dropped, `teardown` never runs, and the mailbox stays registered, so routed deliveries shed unobserved. No death detection. See [RFC](../rfcs/node-failure-handling.md). | High |
| Mailbox capacity is a hardcoded `mailbox(32)` in `spawn_with_caps`; not configurable per-node or per-graph | Medium |
| A long-running `handle` runs to completion — there is no mid-call interruption (cancellation is between messages, via mailbox close) | Medium |

### `fuchsia-engine`

| Gap | Priority |
|-----|----------|
| `add_edge` enforces acyclicity with a full reachability walk per edge (O(V+E)) — fine at workflow scale. If graphs ever grow large, switch to incremental topological maintenance (keep a topo order, check on insert). See [DAG enforcement](../rfcs/dag-enforcement.md). | Low |
| Routing sheds on a full downstream mailbox (at-most-once) with no per-target backpressure option | Low (intentional for the conditioning path; revisit if a lossless route is needed) |

### `fuchsia-actor-wasm`

| Gap | Priority |
|-----|----------|
| No epoch/fuel ticker wired by default — a custom `Config` can enable it, but `WasmActorCreator::new` builds a plain engine, so hard deadlines on in-flight calls don't fire out of the box | Medium |

## Open Questions

| Question | Context |
|----------|---------|
| Should `ActorContext` ids be `Arc<str>`? | Per-message `node_id.clone()` shows up in the guest hosts. Trivial individually; could compound. Not yet profiled. |
| Machine-readable schema for actor configs | Each actor dictates its own `settings` type; no schema for tooling/plugin-store UI. Could derive via `schemars`. |
| Replay / in-flight inspection | Should the runtime support observing messages in mailboxes for debugging? |
| Routing counters' surface | The engine now tracks per-`(node, port)` `delivered`/`shed`/`no_route` counts in-process ([named output ports](../rfcs/output-ports.md)); whether they graduate to a metrics/trace export is a later observability decision. |

## Housekeeping

| Item | Priority |
|------|----------|
| The `fuchsia-actor-wasm` integration test and `fuchsia-examples` need `test-components/actor-echo` built first (they skip / print instructions otherwise); a CI step should build it so the wasm path is actually exercised | Medium |
