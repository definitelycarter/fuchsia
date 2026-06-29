# Lua Actors

`fuchsia-actor-lua` hosts Lua scripts as Fuchsia actors. A `LuaActor<H>` drives a
script's lifecycle on the handle-per-message model: the runtime owns the receive
loop and calls the actor's `setup` / `handle` / `teardown`, each of
which calls into a persistent `mlua::Lua` VM. The VM is built once in `setup` and
reused for every `handle` — globals, upvalues, and module state stay live across
messages, until `teardown`.

The shape mirrors [`fuchsia-actor-wasm`](./wasm.md) — same trait surface, same
per-runtime creator, same synchronous *guest* contract — without WIT or bindgen.

## Synchronous guest, async host

The Lua script contract is **synchronous from the script's point of view**: a
script author writes straight-line synchronous code, and the `emit` global is a
synchronous call (`emit` is a non-blocking channel `offer`). The host drives
that synchronous script through **mlua async** (`call_async`), so a guest call
can suspend while an async host import runs and then resume. `emit` staying
synchronous keeps the emit path cheap (a non-blocking offer); it does not mean
the host is synchronous. Capabilities that are inherently async are a product
host's concern, exactly as on the Wasm side.

## Generic over the binding set

`LuaActor` is generic over [`LuaHost`](../architecture/host-extensibility.md) —
the analog of the Wasm side's `add_to_linker`, just without WIT. A host populates
the Lua state with whatever globals its scripts may call. The crate ships
**`BaseLuaHost`**, which registers only the contract `emit` global.

```rust
pub struct LuaActor<H: LuaHost> { /* source, host, emit, lua */ }
```

## The creator: one per runtime

A `LuaActorCreator<H>` is registered once under the type name `"lua"`. It owns a
catalog of script sources keyed by id; each `create` reads the script id from
`ActorConfig.env` (under `COMPONENT_ENV_KEY`, `"component"`) and builds an actor.

```rust
use fuchsia_actor_lua::{BaseLuaHost, LuaActorCreator};

let creator = LuaActorCreator::new(BaseLuaHost::new())
    .with_source("convert", CONVERT_LUA);     // or .with_path("convert", "scripts/convert.lua")?

engine.register("lua", creator).await;
```

There's no `Engine` to share; the host carries whatever the script will use.

## The contract

A script must define a global `handle(ctx, msg)`. `setup(ctx)` and
`teardown(ctx)` are optional — the runtime skips them if undefined, but a missing
`handle` fails at construction.

```lua
function setup(ctx)        -- optional; runs once before the first message
  -- open connections, prepare state
end

function handle(ctx, msg)
  -- ctx: { execution_id, node_id, task_id } — execution_id is the run's
  --   correlation id (set by the host; the script reads it, never threads it)
  -- msg: { type = "...", value = { kind = "json"|"binary"|"empty", data = ... } }
  emit({                              -- default "out" port
    type = "processed",
    value = { kind = "json", data = '{"ok": true}' }
  })
  emit_to("true", {                   -- a named output port
    type = "branch",
    value = { kind = "empty" }
  })
end

function teardown(ctx)     -- optional; runs on mailbox close
end
```

Inbound messages arrive as a Lua table mirroring `MessageValue`: `kind` is
`"json"` (with `data` the JSON text), `"binary"` (with `data` a Lua string of the
bytes), or `"empty"`. Outbound emissions are the same shape, passed to `emit`
(the default `"out"` port) or `emit_to(port, msg)` (a **named output port**),
which the host converts to a `Message` before forwarding. Each emission reaches
only the successors wired to its port (see
[named output ports](../rfcs/output-ports.md)).

## Actor lifecycle

When the runtime builds and starts the actor:

1. A fresh `mlua::Lua` is created (the `send` feature is on, so the VM is `Send`
   and moves into the actor's task).
2. `host.populate(&lua, emit)` registers globals (`emit` / `emit_to` for the
   base host; a product host adds more).
3. The script source is loaded and executed, defining its functions and
   module-level state.
4. The runtime verifies `handle` exists, then calls `setup(ctx)` if defined.
5. Per message: the `Message` is converted to a Lua table and `handle(ctx, msg)`
   is called; the script pushes emissions via `emit(...)` during it.
6. On mailbox close, `teardown(ctx)` runs if defined, before the VM is dropped.

VM state **persists** across messages — scripts can keep caches, counters, or
handles in globals/upvalues. A `handle` call runs to completion (not interrupted
mid-flight).

## `BaseLuaHost`

`BaseLuaHost` registers exactly one global, `emit`:

```lua
emit({ type = "event", value = { kind = "json", data = '{"key":"value"}' } })
```

`kind` is `"json" | "binary" | "empty"`. The emission is best-effort (a
non-blocking offer), so `emit` always returns successfully. No `log` or `http` is
registered — those are product capabilities. To add globals, write your own
`LuaHost`; see [Host Extensibility](../architecture/host-extensibility.md).

## Note on the `mlua` version

`mlua` uses the `vendored` feature, which builds and statically links its own
copy of the native `lua` library. Because that native lib can appear only once
in a build, if another crate ever also links `lua`, the whole dependency graph
must agree on a single `mlua` version.

## Test

The integration test (`crates/fuchsia-actor-lua/tests/lua_actor.rs`) registers a
`LuaActorCreator<BaseLuaHost>` as the `"lua"` runtime, provisions a two-node
graph (lua echo → native recorder) on the engine, pushes a message, and asserts
the script echoed it onward through `emit`.
