# Capabilities

A **capability** is host-provided functionality that actors can use:
making HTTP requests, reading from a key-value store, publishing to MQTT,
talking to a device pool. Fuchsia ships exactly the capabilities that are
universal enough to standardize — HTTP and log routing — and leaves
everything else to the host.

## Universal: `fuchsia-capabilities`

The `fuchsia-capabilities` crate currently exposes one trait:

```rust
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
}
```

Plus value types (`HttpRequest`, `HttpResponse`, `HttpError`), an
`AllowedHosts` policy (exact + wildcard-prefix matching), and a
`ReqwestHttp` default implementation built on `reqwest`.

```rust
use fuchsia_capabilities::http::{AllowedHosts, ReqwestHttp};

let http: Arc<dyn HttpClient> = Arc::new(
    ReqwestHttp::new(AllowedHosts::new(["api.example.com", "*.googleapis.com"]))
);
```

The `AllowedHosts` check happens *inside* the client — any request whose
URL's host doesn't match returns `HttpError::HostNotAllowed`. That means
the actor calling `http.send(...)` doesn't decide what's allowed; the host
that constructed the `HttpClient` already did.

## Logging: tracing, not a capability

`fuchsia-capabilities` does **not** define a `LogClient` trait. Log
routing in Fuchsia goes through [`tracing`] directly, and `tracing`
already abstracts where logs go (the consuming application installs the
subscriber — stdout, JSON, OTLP, journald, whatever).

- **Inside Rust actors:** just call `tracing::info!`, `tracing::debug!`,
  etc. The orchestrator wraps each actor's task in a span containing the
  `node` id and `kind`, so events automatically pick up that context.
- **Inside Wasm components:** the WIT interface `fuchsia:log/log`
  provides a `log(level, message)` function. The `DefaultHost` in
  `fuchsia-actor-wasm` implements it by routing to `tracing` under the
  target `"wasm.component"`.
- **Inside Lua scripts:** the global `log` table provides
  `log.log(level, message)`. The `DefaultLuaHost` in `fuchsia-actor-lua`
  routes the same way under the target `"lua.actor"`.

There's no Rust trait because there's no choice for the host to make at
the trait layer — `tracing` *is* the abstraction. The choice is which
subscriber to install, and that lives at the application boundary.

[`tracing`]: https://docs.rs/tracing

## Emit: routing component output to the channel

Native Rust actors receive an `Emitter` as a `run` argument and call
`.send(value).await` directly. Wasm and Lua actors can't — they don't
have access to the Rust-side channel — so emission flows through a host
capability instead:

- **Inside Wasm components:** the WIT interface `fuchsia:actor/emit`
  provides a `send(msg: payload) -> result<_, string>` function. The
  `DefaultHost` in `fuchsia-actor-wasm` implements it by converting the
  WIT `payload` to a `Message` and calling the actor's `Emitter::send`.
  Returns `Err("channel closed")` to the component if every downstream is
  gone.
- **Inside Lua scripts:** the global `emit(msg)` function does the same,
  registered by `DefaultLuaHost`. `msg` is a Lua table `{ type = "...",
  value = { kind = "json"|"binary"|"empty", data = ... } }`. Calling
  `emit(...)` with a closed downstream raises a Lua error.

Like log, emit is *universal infrastructure* rather than a configurable
capability — every actor world needs to be able to push downstream,
regardless of what else the host wires in. Custom `WasmHost` / `LuaHost`
implementations must include the emit wiring; see
[Host Extensibility](./host-extensibility.md).

## The injection pattern

Capabilities are injected at **actor registration time**, not at
workflow-start time. The factory closure registered with
`ActorRegistry::register` captures whatever handles the actor needs:

```rust
let http = Arc::new(ReqwestHttp::new(AllowedHosts::new(["api.example.com"])));

registry.register::<MyApiCaller, MyConfig, _>("my.api-caller", move |cfg| {
    MyApiCaller::new(http.clone(), cfg.endpoint)
});
```

Two important properties:

1. **The actor doesn't pick its capability impl.** It receives an
   `Arc<dyn HttpClient>` and uses it. The host decides whether that's
   `ReqwestHttp` configured with one allow-list, with another, or a mock
   for tests.
2. **The capability is per-host, not per-actor.** Two actors registered
   in the same host typically share the same `Arc<dyn HttpClient>`. That
   means the allow-list is uniform within a host — by design. If you
   want different allow-lists for different actor classes, build that
   into the host by registering them with different `Arc`s.

## Domain capabilities

For anything beyond HTTP and log, the host defines its own traits.
Common examples for an IoT host:

- `KvStore` — a key-value store (Redis, sled, local cache)
- `MqttPublisher` — fan-out to an MQTT broker
- `ModbusClient` — talk to a PLC
- `DevicePool<T>` — generic resource pool for any protocol-specific
  client

These traits live in the host's own crates. The host wires them into
actors the same way — `Arc<dyn KvStore>` captured by the factory closure.
For Wasm and Lua actors, the host also needs to expose them through the
language boundary; that's covered in [Host Extensibility](./host-extensibility.md).

## Why not include KV / config in the core?

We considered it. We chose not to, for two reasons:

- **Different hosts want different shapes.** A KV store interface that
  works for an embedded host (single-process, in-memory) is different
  from one for a distributed host (Redis with TTLs and CAS). Picking one
  in the core forces a wrong fit everywhere.
- **The injection pattern doesn't need a centralized trait.** As long as
  the host can hand `Arc<dyn SomeTrait>` to its actors, where the trait
  is defined matters very little. Hosts can standardize on shared traits
  among themselves if/when convergence emerges, without Fuchsia
  pre-empting the shape.

HTTP is special because it's *universal* (every IoT host wants outbound
HTTP somewhere) and its trait shape is uncontroversial. KV isn't.
