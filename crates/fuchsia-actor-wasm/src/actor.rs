use crate::host::WasmHost;
use async_trait::async_trait;
use fuchsia_actor::{Actor, ActorError, Context, Emitter, Inbox};
use std::sync::Arc;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};

/// A [`fuchsia_actor::Actor`] backed by a wasm component.
///
/// `WasmActor` is generic over a [`WasmHost`] that supplies the world-specific
/// concerns (state shape, bindings type, import wiring, lifecycle trampolines).
/// Fuchsia ships [`DefaultHost`](crate::DefaultHost) for the canonical
/// `actor-component` world; hosts with custom capabilities define their own.
///
/// Per actor instance: one `Store<H::State>` is created at the top of `run`,
/// the component is instantiated against the pre-built `Linker`, and the
/// host trampolines drive `setup` → loop(`handle` per inbound message) →
/// `teardown` on cancellation. The component pushes downstream payloads via
/// the host-side `emit` import.
///
/// Cheap to clone — `engine`, `component`, `linker`, and `host` are all
/// `Arc`-backed (or have `Arc` semantics in their respective types). Each
/// clone produces an independent actor with its own store when run.
pub struct WasmActor<H: WasmHost> {
  pub(crate) engine: Engine,
  pub(crate) component: Component,
  pub(crate) linker: Arc<Linker<H::State>>,
  pub(crate) host: Arc<H>,
  pub(crate) epoch_deadline: u64,
}

impl<H: WasmHost> Clone for WasmActor<H> {
  fn clone(&self) -> Self {
    Self {
      engine: self.engine.clone(),
      component: self.component.clone(),
      linker: Arc::clone(&self.linker),
      host: Arc::clone(&self.host),
      epoch_deadline: self.epoch_deadline,
    }
  }
}

impl<H: WasmHost> WasmActor<H> {
  /// Start building a `WasmActor` with the given engine and host.
  pub fn builder(engine: Engine, host: H) -> crate::WasmActorBuilder<H> {
    crate::WasmActorBuilder::new(engine, host)
  }
}

#[async_trait]
impl<H: WasmHost> Actor for WasmActor<H> {
  async fn run(&self, mut inbox: Inbox, emit: Emitter, ctx: Context) -> Result<(), ActorError> {
    let mut store = Store::new(&self.engine, self.host.initial_state(emit));
    store.set_epoch_deadline(self.epoch_deadline);

    let bindings = self
      .host
      .instantiate(&mut store, &self.component, &self.linker)
      .await
      .map_err(|e| ActorError::Other(format!("wasm instantiation failed: {e}")))?;

    match self.host.call_setup(&bindings, &mut store, &ctx).await {
      Err(e) => return Err(ActorError::Other(format!("wasm trap (setup): {e}"))),
      Ok(Err(msg)) => return Err(ActorError::Other(format!("component setup error: {msg}"))),
      Ok(Ok(())) => {}
    }

    let loop_result: Result<(), ActorError> = loop {
      let msg = tokio::select! {
        _ = ctx.cancelled() => break Ok(()),
        msg = inbox.recv() => msg,
      };

      let Some(msg) = msg else {
        break Ok(());
      };

      match self
        .host
        .call_handle(&bindings, &mut store, &ctx, &msg)
        .await
      {
        Err(e) => break Err(ActorError::Other(format!("wasm trap (handle): {e}"))),
        Ok(Err(msg)) => break Err(ActorError::Other(format!("component handle error: {msg}"))),
        Ok(Ok(())) => {}
      }
    };

    match self.host.call_teardown(&bindings, &mut store, &ctx).await {
      Err(e) => tracing::warn!(error = %e, "wasm trap during teardown"),
      Ok(Err(msg)) => tracing::warn!(error = %msg, "component teardown error"),
      Ok(Ok(())) => {}
    }

    loop_result
  }
}
