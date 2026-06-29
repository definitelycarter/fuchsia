# Builtin Actors

`fuchsia-actor-builtins` ships the native Rust actors Fuchsia provides
out of the box — plain `Actor` impls, no Wasm or Lua, registered under canonical
type names. They cover two shapes: a generic **conditioning** pipeline (turning a
noisy input stream into a clean one) and generic **branching** over
[named output ports](../rfcs/output-ports.md) (`if` / `switch`).

Register them all into an `ActorFactory` (or one at a time on the engine):

```rust
fuchsia_actor_builtins::register(&mut factory);
// passthrough · debounce · deadband · dedup · if · switch
```

Each builtin reads its typed config from the node's opaque `settings` document
(via `from_settings`), so a malformed setting fails at construction — i.e. at
provision time, not mid-stream.

## passthrough

Forwards every message unchanged. The simplest node that still exercises the
full receive→emit path — useful for wiring up and debugging a graph before real
operators exist. Takes no settings; needs only `emit`.

## dedup

Drops consecutive duplicate **values**: emits only when a message's `value`
differs from the previously emitted one. It compares the value, not the `type_`
(the type is the constant event discriminator; the value is what "changed or
not" is about). Takes no settings.

## deadband

Suppresses changes below a threshold: emits a numeric reading only when it
differs from the last *emitted* value by at least `threshold`. Comparing against
the last emitted value (not the last seen) stops a slow drift from accumulating
silently — each emission resets the reference point. Non-numeric messages pass
through untouched.

```json
{ "threshold": 0.5 }
```

## debounce

Trailing-edge debounce: holds the most recent value and emits it once input has
been quiet for `delay_ms`. Each input re-arms the quiet timer. An optional
`max_wait_ms` bounds starvation on a never-quiet stream — the latest value is
emitted at least that often even if the quiet window never elapses.

```json
{ "delay_ms": 500, "max_wait_ms": 5000 }
```

Debounce is the canonical user of the **`schedule`** capability: each input bumps
a generation counter and schedules a timer tagged with it; a timer left stale by
a newer input sees the mismatch and drops. Re-arming is cancellation-free
because [`schedule`](../architecture/host-capabilities.md) is fire-and-forget.

## A conditioning pipeline

These compose into a typical conditioning shape:

```text
ingress → dedup → deadband → debounce → (out)
```

— drop exact repeats, ignore sub-threshold jitter, settle bursts, then emit the
result. Each stage is its own node, so a workflow author rearranges or omits
them in the graph without touching code. Where the result *goes* — a durable
state write, an HTTP call — is a product-defined terminal node, not a fuchsia
builtin.

## Branching: `if` and `switch`

Where the conditioning operators are unary (one in, one out), `if` and `switch`
are the first **multi-output** builtins: a generic evaluator written once, each
*use* configured per node, that routes each input to one of several
[named output ports](../rfcs/output-ports.md). The predicate lives in the node's
`settings`, not in code.

### if

Evaluates a `Condition` over the payload and forwards the message unchanged on
the `"true"` or `"false"` port (always those two — `output_ports` is
`Fixed(["true", "false"])`). The condition is the whole `settings` body:

```json
{ "field": "temp", "op": "gt", "value": 30 }
```

### switch

Extracts a `key` field from the payload and forwards on the matching case port,
falling back to `"default"`. Listing the cases *is* configuring the node's ports:
its `output_ports` is `Fixed(cases + ["default"])`.

```json
{ "key": "kind", "cases": ["temp", "humidity"] }
```

— ports `temp`, `humidity`, `default`. Editing `cases` changes the node's ports.

### The `Condition` enum

Both arms ship behind one `#[serde(untagged)]` enum, so a product picks the shape
without a re-design (the declarative arm covers the Home Assistant path, the
`expr` arm the n8n path):

- **Declarative** (the tagless default) — `{ field, op, value }`, with `op` one
  of `eq` / `ne` / `gt` / `gte` / `lt` / `lte`, combinable into `all` / `any`
  groups. A missing field (or a non-JSON payload) makes a leaf `false`. Ordering
  ops are numeric-only.

  ```json
  { "all": [
    { "field": "temp", "op": "gt", "value": 30 },
    { "field": "humidity", "op": "lt", "value": 50 }
  ] }
  ```

- **`expr`** — a [minijinja](https://docs.rs/minijinja) expression string, e.g.
  `{ "expr": "temp > 30" }`. The payload object's top-level keys are the
  expression's variables (`{ "sensor": { "temp": 100 } }` makes `sensor.temp`
  readable); a missing field is `undefined` and a comparison against it is
  `false` (matching the declarative arm, *not* an error). A syntactically invalid
  expression surfaces as a config error per message.
