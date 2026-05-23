# Lua Runtime

> **Status: Done** — `fuchsia-task-runtime-lua`

The Lua runtime executes nodes as Lua scripts via [mlua](https://github.com/khvzak/mlua).

## Why Lua

- Lightweight and fast — minimal overhead for simple transformations
- Easy to embed in Rust via mlua
- Familiar scripting language for data transformation and glue logic
- Sandboxable — globals can be selectively exposed

## Script Interface

A Lua component must define an `execute(ctx, data)` function:

```lua
function execute(ctx, data)
    -- ctx: table with host capability functions
    -- data: JSON string of the resolved input

    -- Parse input
    local input = json_decode(data)

    -- Do work
    local result = {
        greeting = "Hello, " .. (input.name or "world") .. "!",
        processed = true
    }

    -- Return JSON string
    return json_encode(result)
end
```

- **`ctx`** — Table with host capability functions (KV, config, log, HTTP)
- **`data`** — JSON string of the node's resolved input
- **Return value** — JSON string of the node's output

## Host Capability Wiring

Capabilities are exposed through the `ctx` table:

```lua
function execute(ctx, data)
    -- KV store
    ctx.kv_set("my_key", "my_value")
    local value = ctx.kv_get("my_key")
    ctx.kv_delete("my_key")

    -- Config
    local api_key = ctx.config_get("api_key")

    -- Logging
    ctx.log("info", "processing item")

    -- HTTP
    local response = ctx.http_request("GET", "https://api.example.com/data", "{}")

    return data
end
```

Each function is a thin Rust closure that calls the shared host capability implementation. No capability logic is duplicated.

## Architecture

```rust
pub struct LuaExecutor;

impl NodeExecutor for LuaExecutor {
    fn execute(&self, bytes: &[u8], capabilities: Capabilities, input: TaskInput)
        -> Result<TaskOutput, ExecutorError>
    {
        // 1. Parse bytes as UTF-8 Lua source
        // 2. Create fresh Lua VM
        // 3. Inject capability bindings as ctx table
        // 4. Load and execute script
        // 5. Call execute(ctx, json_input)
        // 6. Parse return value as JSON
    }
}
```

## Sandboxing

The Lua VM is configured to:
- Remove dangerous standard library modules (`os`, `io`, `loadfile`, `dofile`)
- Only expose declared capability functions through the `ctx` table
- Create a fresh VM per invocation — no state leakage between calls

## Open Questions

- **Caching**: Should Lua bytecode be cached, or is source loading fast enough?
- **Async**: How to handle async host capabilities (HTTP, KV) from synchronous Lua? Currently uses `block_on` for simplicity.
- **Module system**: Should components be able to `require()` other Lua modules from the package?
