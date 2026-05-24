use async_trait::async_trait;
use fuchsia_actor::{Context, Emitter, Message};
use wasmtime::Store;
use wasmtime::component::{Component, Linker};
use wasmtime_wasi::WasiView;

/// Glue between [`WasmActor`](crate::WasmActor) and a host-specific wasm world.
///
/// A `WasmHost` impl owns the world-specific concerns — generated bindings,
/// per-Store state, linker wiring, and the trampolines that call the
/// component's lifecycle exports (`setup`, `handle`, `teardown`). The actor
/// crate provides the channel loop and lifecycle orchestration; the host
/// fills in the world-specific gaps.
///
/// Fuchsia ships [`DefaultHost`](crate::DefaultHost) for the canonical
/// `actor-component` world (log + http + emit). Hosts with additional
/// capabilities (MQTT, BLE, etc.) define their own WIT world and implement
/// `WasmHost` for the corresponding bindgen output.
#[async_trait]
pub trait WasmHost: 'static + Send + Sync {
  /// Per-actor `Store` state. Holds the `WasiCtx`, capability handles, the
  /// downstream `Emitter`, and any host-specific bookkeeping. Persists for
  /// the lifetime of the actor — built once by [`initial_state`] when the
  /// actor starts running, *not* per message.
  ///
  /// Hosts must stash the `Emitter` somewhere the `emit` import callback
  /// (wired in [`add_to_linker`]) can reach — typically a struct field.
  ///
  /// [`initial_state`]: WasmHost::initial_state
  /// [`add_to_linker`]: WasmHost::add_to_linker
  type State: WasiView + 'static + Send;

  /// Typed bindings returned by `bindgen!`. The exact type is opaque to the
  /// actor crate; the host produces it in [`instantiate`] and consumes it
  /// in the [`call_setup`] / [`call_handle`] / [`call_teardown`] trampolines.
  ///
  /// [`instantiate`]: WasmHost::instantiate
  /// [`call_setup`]: WasmHost::call_setup
  /// [`call_handle`]: WasmHost::call_handle
  /// [`call_teardown`]: WasmHost::call_teardown
  type Bindings: Send;

  /// Wire host functions into the linker so the component's imports are
  /// satisfied. Called once at builder time. Implementations must wire
  /// the `fuchsia:actor/emit` import alongside any other host imports.
  fn add_to_linker(&self, linker: &mut Linker<Self::State>) -> wasmtime::Result<()>;

  /// Build the per-actor `State`. Called once when the actor starts running.
  /// The provided `Emitter` is the actor's outbound channel — implementations
  /// must store it where the emit import callback can find it.
  fn initial_state(&self, emitter: Emitter) -> Self::State;

  /// Instantiate the component into the store using the (pre-built) linker.
  /// Called once at the top of the actor's run loop. The returned bindings
  /// are reused across every `setup` / `handle` / `teardown` call for the
  /// life of the actor.
  async fn instantiate(
    &self,
    store: &mut Store<Self::State>,
    component: &Component,
    linker: &Linker<Self::State>,
  ) -> wasmtime::Result<Self::Bindings>;

  /// Invoke the component's `actor.setup` export. Called once before the
  /// message loop. Returns the component-reported result.
  async fn call_setup(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &Context,
  ) -> wasmtime::Result<Result<(), String>>;

  /// Invoke the component's `actor.handle` export. Called once per inbound
  /// message. The component pushes downstream emissions via the host-side
  /// `emit` import; this function does not return a payload.
  async fn call_handle(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &Context,
    msg: &Message,
  ) -> wasmtime::Result<Result<(), String>>;

  /// Invoke the component's `actor.teardown` export. Called once on
  /// cancellation or inbox close, before the store is dropped. Errors are
  /// logged and swallowed by the runtime — the actor is shutting down
  /// either way.
  async fn call_teardown(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &Context,
  ) -> wasmtime::Result<Result<(), String>>;
}
