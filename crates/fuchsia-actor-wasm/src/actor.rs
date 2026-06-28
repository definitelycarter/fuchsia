use std::sync::Arc;

use fuchsia_actor::{Actor, ActorContext, ActorError, Emit, Message, async_trait};
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

use crate::host::WasmHost;

/// A [`fuchsia_actor::Actor`] backed by a wasm component.
///
/// Generic over a [`WasmHost`] that supplies the world-specific wiring. The
/// engine, component, host, and emit handle are captured at construction
/// (cheap `Arc`-style clones); the heavy state — the `Store` and the
/// instantiated component bindings — is built in [`setup`](Actor::setup) and
/// reused for every [`handle`](Actor::handle) until [`teardown`](Actor::teardown).
pub struct WasmActor<H: WasmHost> {
  engine: Engine,
  component: Component,
  host: Arc<H>,
  emit: Arc<dyn Emit>,
  // Built in `setup`. `store` and `bindings` are separate fields so `handle`
  // can borrow the store mutably and the bindings immutably at once.
  store: Option<Store<H::State>>,
  bindings: Option<H::Bindings>,
}

impl<H: WasmHost> WasmActor<H> {
  /// Build an actor for `component`, driven through `host`, emitting via `emit`.
  /// `engine` is shared across all actors built from the same creator (an
  /// `Engine` clone is a refcount bump).
  pub fn new(engine: Engine, component: Component, host: Arc<H>, emit: Arc<dyn Emit>) -> Self {
    Self {
      engine,
      component,
      host,
      emit,
      store: None,
      bindings: None,
    }
  }
}

#[async_trait]
impl<H: WasmHost> Actor for WasmActor<H> {
  async fn setup(&mut self, ctx: &ActorContext) -> Result<(), ActorError> {
    let mut linker = Linker::<H::State>::new(&self.engine);
    // Trap first on the empty linker so *every* component import (including the
    // WASI a guest drags in but never calls) becomes a trap; then let the host
    // wire the real `emit`/contract imports on top. Shadowing lets those real
    // definitions override the placeholder traps. Done in this order so a real
    // import can never be clobbered by a trap.
    if self.host.trap_unknown_imports() {
      linker.allow_shadowing(true);
      linker
        .define_unknown_imports_as_traps(&self.component)
        .map_err(|e| ActorError::Setup(format!("trap unknown imports: {e}")))?;
    }
    self
      .host
      .add_to_linker(&mut linker)
      .map_err(|e| ActorError::Setup(format!("link host imports: {e}")))?;

    // `Arc::clone` of the emit handle is a refcount bump — the store needs its
    // own owned handle for the lifetime of the component instance.
    let mut store = Store::new(
      &self.engine,
      self.host.initial_state(Arc::clone(&self.emit)),
    );

    let bindings = self
      .host
      .instantiate(&mut store, &self.component, &linker)
      .map_err(|e| ActorError::Setup(format!("instantiate component: {e}")))?;

    match self.host.call_setup(&bindings, &mut store, ctx) {
      Ok(Ok(())) => {}
      Ok(Err(msg)) => return Err(ActorError::Setup(format!("component setup: {msg}"))),
      Err(e) => return Err(ActorError::Setup(format!("trap in setup: {e}"))),
    }

    self.store = Some(store);
    self.bindings = Some(bindings);
    Ok(())
  }

  async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let store = self
      .store
      .as_mut()
      .ok_or_else(|| ActorError::Handle("component not instantiated".to_owned()))?;
    let bindings = self
      .bindings
      .as_ref()
      .ok_or_else(|| ActorError::Handle("component not instantiated".to_owned()))?;

    match self.host.call_handle(bindings, store, ctx, &msg) {
      Ok(Ok(())) => Ok(()),
      Ok(Err(msg)) => Err(ActorError::Handle(format!("component handle: {msg}"))),
      Err(e) => Err(ActorError::Handle(format!("trap in handle: {e}"))),
    }
  }

  async fn teardown(&mut self, ctx: &ActorContext) -> Result<(), ActorError> {
    // Teardown is best-effort: if the component never instantiated (setup
    // failed) there is nothing to tear down.
    let (Some(store), Some(bindings)) = (self.store.as_mut(), self.bindings.as_ref()) else {
      return Ok(());
    };

    match self.host.call_teardown(bindings, store, ctx) {
      Ok(Ok(())) => {}
      Ok(Err(msg)) => tracing::warn!(error = %msg, "component teardown error"),
      Err(e) => tracing::warn!(error = %e, "trap during component teardown"),
    }
    Ok(())
  }
}
