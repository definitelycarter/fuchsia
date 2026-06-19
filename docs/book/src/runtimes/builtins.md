# Builtin Actors

`fuchsia-actor-builtins` ships the native Rust actors Fuchsia provides
out of the box — plain `Actor` impls, no Wasm or Lua, registered under canonical
type names. They cover the conditioning pipeline that turns a noisy input stream
into a clean, committed value.

Register them all into an `ActorFactory` (or one at a time on the engine):

```rust
fuchsia_actor_builtins::register(&mut factory);
// passthrough · debounce · deadband · dedup · commit
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

## commit

The terminal node of an entity's pre-write pipeline: writes a conditioned value
to entity state through the **`state`** capability. Everything upstream
(debounce/deadband/dedup) is best-effort and lossy; the commit is the durable
write that downstream automation hangs off.

It holds a `StateSink` the host pre-scoped to the entity's storage and never
learns where the value lands — the same neighbor-ignorance as `emit`. Because a
silently dropped state write would hide a misconfiguration, `commit` **fails
construction** if no `state` sink was granted (there's no no-op fallback).
`MessageValue::Json` is serialized to BSON, `Binary` is stored as a binary blob,
`Empty` becomes `Null`.

## A conditioning pipeline

These compose into the typical shape behind one entity reading:

```text
ingress → dedup → deadband → debounce → commit → (state)
```

— drop exact repeats, ignore sub-threshold jitter, settle bursts, then write the
result. Each stage is its own node, so a workflow author rearranges or omits
them in the graph without touching code.
