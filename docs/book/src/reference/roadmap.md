# Roadmap

Outstanding features, known gaps, and open questions. Rows are removed when work
lands (no strikethrough).

## Features

| Feature | Description | Notes |
|---------|-------------|-------|
| Per-actor retry policy | Configurable retries with backoff around a node's `handle`, beyond the at-least-once feeder's retry-on-loss | `fuchsia-runtime` / `fuchsia-engine` |
| More conditioning operators | Throttle, window, threshold-over-time to round out the existing `debounce`/`deadband`/`dedup` set | `fuchsia-actor-builtins` |
| Config import for guests | Forward a `Component` node's `settings` into a Wasm/Lua guest (e.g. a `config.get(key)` import). Today only native actors read `settings`; guests receive only `ctx` + payload. | `fuchsia-actor-wasm`, `fuchsia-actor-lua` |
| Workflow-level allowlist of actor types | Workflows declare which type names they may instantiate; the factory/provisioner rejects unknown | `fuchsia-actor`, `fuchsia-provisioner` |
| Capability-style device binding | Bind each actor instance to one host-side device handle (BLE/MQTT/…) so guest-side functions never name addresses | host crates, per-capability WIT |
| Cycle support / defined back-edge semantics | Specify behavior for back-edges within a graph (persistent actor lifecycles already work — this is about graph shape) | `fuchsia-engine` |
| Distributed actors | Patterns + sample host code for splitting a graph across processes via transport actors | likely host docs, not core |

## Gaps

### `fuchsia-transport` / `fuchsia-runtime`

| Gap | Priority |
|-----|----------|
| Mailbox capacity is a hardcoded `mailbox(32)` in `spawn_with_caps`; not configurable per-node or per-graph | Medium |
| A long-running `handle` runs to completion — there is no mid-call interruption (cancellation is between messages, via mailbox close) | Medium |

### `fuchsia-engine`

| Gap | Priority |
|-----|----------|
| No cycle detection when adding edges; back-edge behavior is unspecified | Medium |
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
| Workflow-level capability declarations | If a type-name allowlist lands, should it extend to declaring which capabilities each node may be granted? |
| Replay / in-flight inspection | Should the runtime support observing messages in mailboxes for debugging? |

## Housekeeping

| Item | Priority |
|------|----------|
| The `fuchsia-actor-wasm` integration test and `fuchsia-examples` need `test-components/actor-echo` built first (they skip / print instructions otherwise); a CI step should build it so the wasm path is actually exercised | Medium |
