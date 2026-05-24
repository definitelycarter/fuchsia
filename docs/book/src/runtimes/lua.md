# Lua Actors

`fuchsia-actor-lua` hosts Lua scripts as Fuchsia actors. Each `LuaActor`
holds a Lua source string and a `LuaHost`. When the actor starts, the
runtime creates one `mlua::Lua` VM, calls `host.populate(&lua, emitter)`
to register globals (including `emit`), loads the script, and drives its
lifecycle: optional `setup()`, `handle(ctx, msg)` per inbound delivery,
optional `teardown()` on shutdown. The Lua VM persists for the
actor's lifetime — globals, upvalues, and module state stay live across
every `handle` call.

The shape mirrors [`fuchsia-actor-wasm`](./wasm.md) — same trait surface,
same builder pattern, same lifecycle contract — without the WIT and
bindgen layers.

## Anatomy

```rust
pub struct LuaActor<H: LuaHost> {
    source: Arc<String>,
    host: Arc<H>,
}
```

`LuaActor: Clone` is cheap (two `Arc`s). Each clone runs its own Lua VM
when started.

## Builder

```rust
use fuchsia_actor_lua::{DefaultLuaHost, LuaActor};
use fuchsia_capabilities::http::{AllowedHosts, ReqwestHttp};

let http = Arc::new(ReqwestHttp::new(AllowedHosts::new(["api.example.com"])));
let host = DefaultLuaHost::new(http);

let actor = LuaActor::builder(host)
    .source(SCRIPT)              // inline string
    // .source_from_path("scripts/transform.lua")  // or from disk
    .build()?;
```

There's no `Engine` to share. The host carries whatever capability handles
the script will access.

## The contract

A Lua actor script must define a global `handle(ctx, msg)` function.
`setup()` and `teardown()` are optional — the runtime skips them if
undefined.

```lua
function setup(ctx)
  -- optional; runs once before the message loop
  -- open connections, subscribe to streams, prepare state
  log.log("info", "setup node " .. ctx.node_id)
end

function handle(ctx, msg)
  -- ctx is a table with fields: node_id, execution_id, task_id
  -- msg is a table: { type = "...", value = { kind = "json"|"binary"|"empty", data = ... } }

  log.log("info", "node " .. ctx.node_id .. " received " .. msg.type)

  -- ... do work ...

  emit({
    type = "processed",
    value = { kind = "json", data = '{"ok": true}' }
  })
end

function teardown(ctx)
  -- optional; runs on cancellation or inbox close
  log.log("info", "teardown node " .. ctx.node_id)
end
```

Inbound messages arrive as a Lua table `{ type = "...", value = { kind =
"json"|"binary"|"empty", data = ... } }` — mirroring the `payload` type
on the Wasm side. Outbound emissions are the same shape: a table passed
to `emit`, which the host converts to a `Message` before forwarding
downstream.

## Actor lifecycle

When the orchestrator spawns the actor:

1. A fresh `mlua::Lua` is created (the `send` feature is enabled, so the
   VM is `Send` and can persist across awaits).
2. `host.populate(&lua, emitter)` registers globals (typically `log`,
   `http`, and `emit` for the default host; custom hosts add more).
3. The script source is loaded and executed, defining any functions and
   module-level state as globals or closures.
4. If `setup` is defined, it's called once with the actor's context.
5. For each inbound message: the actor converts the inbox `Message` to a
   Lua table and calls `handle(ctx, msg)`. The script pushes any outgoing
   payloads via `emit(...)` during the call.
6. On cancellation or inbox close, `teardown(ctx)` runs if defined,
   before the VM is dropped.

Lua VM state *does* persist across messages — upvalues, module-level
locals, and global tables stay live. Scripts can use this to maintain
caches, counters, or open resources between handle invocations.

Cancellation is checked between `handle` calls, not during. A long-running
Lua call cannot be interrupted mid-flight.

## What `DefaultLuaHost` registers

The off-the-shelf `DefaultLuaHost` exposes three globals:

```lua
log.log(level, message)              -- routes to tracing
                                     -- level: "trace" | "debug" | "info" | "warn" | "error"

local resp = http.send({
  method = "POST",
  url = "https://api.example.com/items",
  headers = { ["Content-Type"] = "application/json" },
  body = '{"foo": "bar"}',
})
-- resp.status, resp.headers, resp.body

emit({ type = "event", value = { kind = "json", data = '{"key":"value"}' } })
                                     -- push typed payload downstream
```

`http.send` returns a Lua table on success; on failure it raises a Lua
runtime error with the `HttpError` display string. Wrap calls in
`pcall` if you want to handle failures inside the script.

`emit` raises a Lua error if the downstream channel is closed; uncaught,
that ends the actor.

## Adding capabilities

The `LuaHost` trait is one method — `populate(&Lua, Emitter)`. To add
capabilities, write your own host that registers additional globals. See
[Host Extensibility](../architecture/host-extensibility.md) for a worked
example (registering an `mqtt` global).

## Performance notes

- **VM creation happens once per actor**, not per message. `mlua::Lua::new`
  with `lua54+vendored` is fast (microseconds), and amortizing over the
  actor's lifetime makes the cost negligible.
- **Source is loaded once** at the top of `run`. Module-level locals,
  upvalues, and global tables stay live across every `handle` call.
- **Persistent Lua state across messages** is now available by design.
  Scripts that need to remember things can use globals or upvalues
  directly; data no longer has to round-trip through the inbox or an
  external capability.

## Test

The lua integration test uses an inline script:

```rust
const SCRIPT: &str = r#"
function handle(ctx, msg)
  log.log("info", "lua: node " .. ctx.node_id .. " received type " .. msg.type)
  local data = (msg.value and msg.value.data) or "null"
  emit({
    type = "echo",
    value = { kind = "json", data = string.format('{"echoed": %s, "node": "%s"}', data, ctx.node_id) }
  })
end
"#;
```

End-to-end run through `fuchsia-runtime`:
`crates/fuchsia-actor-lua/tests/lua_actor.rs`.
