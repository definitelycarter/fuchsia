//! [`BaseHost`] — a [`WasmHost`] that satisfies nothing but the `fuchsia:actor`
//! contract: it links the `emit` import and traps any other import the
//! component carries (e.g. unused WASI). It deliberately wires no platform
//! capabilities (no log, no http) — those belong to product-defined hosts and
//! worlds. It is enough to run a component that imports only `fuchsia:actor`.

use std::sync::Arc;

use fuchsia_actor::{ActorContext, Emit, Message, MessageValue};
use wasmtime::Store;
use wasmtime::component::{Component, Linker};

use crate::host::WasmHost;

// The host-side world: export the actor lifecycle, import `emit`. Defined
// inline (not in `wit/`) so the canonical `wit/` ships only the `fuchsia:actor`
// interfaces and no bundled "platform" world — products compose their own.
wasmtime::component::bindgen!({
  inline: r#"
    package fuchsia:base-host@0.1.0;
    world base-actor {
      import fuchsia:actor/emit@0.1.0;
      export fuchsia:actor/actor@0.1.0;
    }
  "#,
  path: "../../wit",
  world: "base-actor",
});

use exports::fuchsia::actor::actor::Context as WitContext;
use fuchsia::actor::types::{Payload, PayloadValue};

/// Per-`Store` state for [`BaseHost`]: just the downstream emit sink the
/// `emit` import forwards into.
pub struct BaseHostState {
  emit: Arc<dyn Emit>,
}

// ---- emit import: forward component emissions to the downstream sink -------

impl fuchsia::actor::emit::Host for BaseHostState {
  fn send(&mut self, msg: Payload) -> Result<(), String> {
    self.emit.emit(from_payload(msg)?);
    Ok(())
  }
}

// ---- types import: shared payload definitions (no functions) --------------

impl fuchsia::actor::types::Host for BaseHostState {}

/// Contract-only [`WasmHost`]: links `emit`, traps everything else.
#[derive(Default)]
pub struct BaseHost;

impl BaseHost {
  pub fn new() -> Self {
    Self
  }
}

impl WasmHost for BaseHost {
  type State = BaseHostState;
  type Bindings = BaseActor;

  fn add_to_linker(&self, linker: &mut Linker<Self::State>) -> wasmtime::Result<()> {
    BaseActor::add_to_linker::<BaseHostState, HasBaseHostState>(linker, |s| s)
  }

  fn initial_state(&self, emit: Arc<dyn Emit>) -> Self::State {
    BaseHostState { emit }
  }

  fn instantiate(
    &self,
    store: &mut Store<Self::State>,
    component: &Component,
    linker: &Linker<Self::State>,
  ) -> wasmtime::Result<Self::Bindings> {
    BaseActor::instantiate(store, component, linker)
  }

  fn call_setup(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &ActorContext,
  ) -> wasmtime::Result<Result<(), String>> {
    bindings
      .fuchsia_actor_actor()
      .call_setup(store, &wit_context(ctx))
  }

  fn call_handle(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &ActorContext,
    msg: &Message,
  ) -> wasmtime::Result<Result<(), String>> {
    let payload = to_payload(msg).map_err(wasmtime::Error::msg)?;
    bindings
      .fuchsia_actor_actor()
      .call_handle(store, &wit_context(ctx), &payload)
  }

  fn call_teardown(
    &self,
    bindings: &Self::Bindings,
    store: &mut Store<Self::State>,
    ctx: &ActorContext,
  ) -> wasmtime::Result<Result<(), String>> {
    bindings
      .fuchsia_actor_actor()
      .call_teardown(store, &wit_context(ctx))
  }
}

/// Marker used to thread `BaseHostState` through bindgen's `add_to_linker`.
struct HasBaseHostState;

impl wasmtime::component::HasData for HasBaseHostState {
  type Data<'a> = &'a mut BaseHostState;
}

fn wit_context(ctx: &ActorContext) -> WitContext {
  WitContext {
    execution_id: ctx.execution_id.clone(),
    node_id: ctx.node_id.clone(),
    task_id: ctx.task_id.clone(),
  }
}

fn to_payload(msg: &Message) -> Result<Payload, String> {
  let value = match &msg.value {
    MessageValue::Json(v) => {
      PayloadValue::Json(serde_json::to_string(v).map_err(|e| format!("encode json: {e}"))?)
    }
    MessageValue::Binary(b) => PayloadValue::Binary(b.clone()),
    MessageValue::Empty => PayloadValue::Empty,
  };
  Ok(Payload {
    type_: msg.type_.clone(),
    value,
  })
}

fn from_payload(p: Payload) -> Result<Message, String> {
  let value = match p.value {
    PayloadValue::Json(s) => {
      MessageValue::Json(serde_json::from_str(&s).map_err(|e| format!("decode json: {e}"))?)
    }
    PayloadValue::Binary(b) => MessageValue::Binary(b),
    PayloadValue::Empty => MessageValue::Empty,
  };
  Ok(Message {
    type_: p.type_,
    value,
  })
}
