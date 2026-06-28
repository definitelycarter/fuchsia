use std::sync::Arc;

use fuchsia_actor::{ActorContext, Emit, Message, async_trait};
use wasmtime::Store;
use wasmtime::component::{Component, Linker};

/// Glue between [`WasmActor`](crate::WasmActor) and a host-specific wasm world.
///
/// A `WasmHost` owns the world-specific concerns — the `bindgen!` output, the
/// per-`Store` state, the linker wiring, and the trampolines that call the
/// component's lifecycle exports. The actor crate provides the lifecycle
/// orchestration (build store, instantiate, drive `setup`/`handle`/`teardown`);
/// the host fills in the world-specific gaps. This is the seam that keeps the
/// crate **generic over the import set**: a product implements `WasmHost` for
/// its own world (its own capability imports) without touching the actor.
///
/// The guest-driving methods (`instantiate`, `call_setup`/`handle`/`teardown`)
/// are `async`: the engine runs with `async_support`, so a guest call can
/// suspend its fiber while an async host import (a product's `fetch`, say) runs,
/// without blocking the runtime thread. Pure setup (`add_to_linker`,
/// `initial_state`, `trap_unknown_imports`) stays synchronous.
///
/// This crate ships [`BaseHost`](crate::BaseHost) for the contract-only world
/// (just `emit`). Hosts with additional capabilities (MQTT, BLE, HTTP, …)
/// implement `WasmHost` themselves over the bindgen output for their world.
#[async_trait]
pub trait WasmHost: 'static + Send + Sync {
  /// Per-actor `Store` state. Holds the downstream [`Emit`] handle (so the
  /// `emit` import callback can reach it) plus any host-specific bookkeeping
  /// (a `WasiCtx`, capability handles). Built once per actor by
  /// [`initial_state`](WasmHost::initial_state), then persisted for the
  /// actor's life — not rebuilt per message.
  type State: 'static + Send;

  /// Typed bindings produced by `bindgen!`. Opaque to the actor crate: the
  /// host produces it in [`instantiate`](WasmHost::instantiate) and consumes
  /// it in the `call_*` trampolines.
  type Bindings: Send;

  /// Wire this world's host-function imports into the linker. Must satisfy at
  /// least the `fuchsia:actor/emit` import; a richer host also wires its own
  /// capability imports here. Called once per actor, before instantiation.
  fn add_to_linker(&self, linker: &mut Linker<Self::State>) -> wasmtime::Result<()>;

  /// Whether to define any imports the component declares but this host did
  /// *not* wire as traps (rather than failing instantiation). Defaults to
  /// `true`: real components routinely drag in WASI imports they never call,
  /// and a contract-only host has no reason to satisfy them — trapping on the
  /// unused import lets the component instantiate while still failing loudly
  /// if it ever actually calls one. A host that wants strict
  /// "every import must be satisfied" instantiation overrides this to `false`.
  fn trap_unknown_imports(&self) -> bool {
    true
  }

  /// Build the per-actor [`State`](WasmHost::State). The `emit` handle is the
  /// actor's outbound sink — implementations must stash it where the `emit`
  /// import callback can find it.
  fn initial_state(&self, emit: Arc<dyn Emit>) -> Self::State;

  /// Instantiate the component into the store using the prepared linker.
  async fn instantiate(
    &self,
    store: &mut Store<Self::State>,
    component: &Component,
    linker: &Linker<Self::State>,
  ) -> wasmtime::Result<Self::Bindings>;

  /// Invoke the component's `actor.setup` export. The outer `Result` is a host
  /// trap; the inner is the component's own `result<_, string>`.
  async fn call_setup(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &ActorContext,
  ) -> wasmtime::Result<Result<(), String>>;

  /// Invoke the component's `actor.handle` export. The component pushes any
  /// downstream emissions through the `emit` import; nothing is returned here.
  async fn call_handle(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &ActorContext,
    msg: &Message,
  ) -> wasmtime::Result<Result<(), String>>;

  /// Invoke the component's `actor.teardown` export. Errors are logged and
  /// swallowed by the actor — it is shutting down regardless.
  async fn call_teardown(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &ActorContext,
  ) -> wasmtime::Result<Result<(), String>>;
}
