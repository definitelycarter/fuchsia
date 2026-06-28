# Host Extensibility

Fuchsia owns exactly one contract: **`fuchsia:actor`** — the lifecycle
(`setup`/`handle`/`teardown`), the host-imported `emit`, and the payload types.
It does **not** prescribe what else a Wasm component or Lua script may call. A
home-automation product might expose `ble` / `mqtt` / `state` imports; an
n8n-style product might expose `webhook` / `http`. Both run on the same engine.

That openness is the `WasmHost` and `LuaHost` seam. The guest crates are
*generic* over it: they link the actor lifecycle plus `emit`, and the product
supplies a host that adds its own capability imports.

For **native** actors there's no seam — a capability is just an `Arc<dyn Trait>`
in the bag (see [Capabilities](./host-capabilities.md)). The seam exists only
because Wasm and Lua need explicit host-side bindings.

## The central constraint

- fuchsia ships the `fuchsia:actor` package in `wit/` and bundles **no platform
  world** — a world that fixes a capability import set belongs to a *product*.
- The base hosts (`BaseHost`, `BaseLuaHost`) satisfy nothing but the contract.
  Each guest crate's base host defines its own world **inline** in `bindgen!`
  (export `actor`, import `emit`) — an implementation detail of the crate, not
  something in `wit/`.
- A product composes its own world = export `fuchsia:actor/actor` + import its
  chosen capabilities, and implements `WasmHost` / `LuaHost` to wire them.

## WasmHost

```rust
pub trait WasmHost: 'static + Send + Sync {
    type State: 'static + Send;
    type Bindings: Send;

    fn add_to_linker(&self, linker: &mut Linker<Self::State>) -> wasmtime::Result<()>;
    fn trap_unknown_imports(&self) -> bool { true }
    fn initial_state(&self, emit: Arc<dyn Emit>) -> Self::State;
    async fn instantiate(&self, store, component, linker) -> wasmtime::Result<Self::Bindings>;
    async fn call_setup(&self, …)    -> wasmtime::Result<Result<(), String>>;
    async fn call_handle(&self, …)   -> wasmtime::Result<Result<(), String>>;
    async fn call_teardown(&self, …) -> wasmtime::Result<Result<(), String>>;
}
```

The lifecycle methods are **async** — the guest contract is synchronous, but
this crate drives it with async wasmtime (`exports: async`, `call_async`) so a
synchronous guest call can suspend its fiber while an async host import runs.
The host owns:

- **The WIT world** components target — the contract plugin authors compile to.
- **The `State`** held in each `wasmtime::Store`: the downstream `Emit` handle
  (so the `emit` import can reach it) plus any product handles and (if the world
  uses WASI) a `WasiCtx`. Built once per actor in `initial_state`.
- **The `Bindings`** — output of `wasmtime::component::bindgen!` for the world.

`WasmActor` provides the rest: build the store, instantiate, marshal `Message`
↔ WIT `payload`, and drive `setup` → `handle` → `teardown`.

`trap_unknown_imports` defaults to `true`: real components routinely drag in
WASI imports they never call, and a contract-only host has no reason to satisfy
them. With it on, the actor defines those unsatisfied imports as **traps** —
the component instantiates, but actually *calling* an unwired import fails
loudly. A host that wants strict "every import must be satisfied" instantiation
overrides it to `false`.

### Sketch: a custom WasmHost adding an HTTP import

```rust
// product world (its own wit/), composed against fuchsia:actor:
//   world n8n-component {
//     import fuchsia:actor/emit@0.1.0;
//     import n8n:http/outbound;
//     export fuchsia:actor/actor@0.1.0;
//   }
wasmtime::component::bindgen!({ path: "wit", world: "n8n-component" });

struct N8nHostState { emit: Arc<dyn Emit>, http: Arc<dyn HttpClient> }

impl fuchsia::actor::emit::Host for N8nHostState { /* forward to self.emit */ }
impl n8n::http::outbound::Host for N8nHostState  { /* delegate to self.http */ }

struct N8nHost { http: Arc<dyn HttpClient> }

impl WasmHost for N8nHost {
    type State = N8nHostState;
    type Bindings = N8nComponent;
    fn add_to_linker(&self, l: &mut Linker<Self::State>) -> wasmtime::Result<()> {
        N8nComponent::add_to_linker::<_, HasState>(l, |s| s)
    }
    fn initial_state(&self, emit: Arc<dyn Emit>) -> Self::State {
        N8nHostState { emit, http: self.http.clone() }
    }
    /* instantiate / call_setup / call_handle / call_teardown:
       call the bindgen-produced N8nComponent methods */
}
```

If the import is inherently async (HTTP via `reqwest`), the host already runs
async wasmtime, so the *product* can wire the import as an async host function
and `.await` it directly — the guest's synchronous call suspends its fiber while
it runs. How the product structures that async work is the product's choice.

### Register it per runtime

Guest actors are registered **one creator per runtime kind**, not per component.
`WasmActorCreator<H>` owns a catalog of compiled components keyed by id; each
node names its component in `ActorConfig.env` under `COMPONENT_ENV_KEY`:

```rust
let creator = WasmActorCreator::with_engine(engine, N8nHost { http })
    .with_path("temp-sensor", "components/temp-sensor.wasm")?;
engine.register("wasm", creator).await;
```

A component built against only `fuchsia:actor` runs under any host; one that
imports `n8n:http/outbound` needs a host that wires it.

## LuaHost

Lua's seam is one method — no WIT, no bindgen:

```rust
pub trait LuaHost: 'static + Send + Sync {
    fn populate(&self, lua: &mlua::Lua, emit: Arc<dyn Emit>) -> mlua::Result<()>;
}
```

`populate` runs once per actor, before the script loads. It registers globals —
typically tables of functions — and must wire the provided `Emit` into an `emit`
global. `BaseLuaHost` registers only `emit`; a product adds its own:

```rust
struct N8nLuaHost { http: Arc<dyn HttpClient> }

impl LuaHost for N8nLuaHost {
    fn populate(&self, lua: &mlua::Lua, emit: Arc<dyn Emit>) -> mlua::Result<()> {
        BaseLuaHost::new().populate(lua, emit)?;           // emit
        let http = self.http.clone();
        let t = lua.create_table()?;
        t.set("send", lua.create_function(move |_, req: mlua::Table| {
            // a product may block_on its own async capability here
            Ok(/* … */)
        })?)?;
        lua.globals().set("http", t)
    }
}
```

Register it the same per-runtime way:

```rust
let creator = LuaActorCreator::new(N8nLuaHost { http })
    .with_source("rename", RENAME_LUA);
engine.register("lua", creator).await;
```

## Naming convention

- `BaseHost` (Wasm) and `BaseLuaHost` — contract-only, shipped by Fuchsia.
- `<Product>Host` / `<Product>LuaHost` — a product host that adds capability
  imports. No enforced convention; this just reads cleanly.

## When to extend vs. write a native actor

1. **Do guests genuinely need this capability?** If only first-party Rust code
   uses it, write a native actor that takes the handle through the bag. The seam
   pays off only when third-party Wasm/Lua also needs access.
2. **Is the shape stable enough to lock into WIT?** WIT changes are
   version-incompatible — old plugins break. Prototype as a native actor; promote
   to a WIT-exposed import once the shape settles.
