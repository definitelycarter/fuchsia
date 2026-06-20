use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::error::ActorError;

#[derive(Clone, Debug, PartialEq)]
pub enum MessageValue {
  Json(serde_json::Value),
  Binary(Vec<u8>),
  Empty,
}

/// Typed message payload. `type_` is the event discriminator (e.g.
/// "gatt/2a6e", "timer_tick"); `value` carries the data.
#[derive(Clone, Debug)]
pub struct Message {
  pub type_: String,
  pub value: MessageValue,
}

impl Message {
  pub fn json(type_: impl Into<String>, value: serde_json::Value) -> Self {
    Self {
      type_: type_.into(),
      value: MessageValue::Json(value),
    }
  }

  pub fn binary(type_: impl Into<String>, bytes: Vec<u8>) -> Self {
    Self {
      type_: type_.into(),
      value: MessageValue::Binary(bytes),
    }
  }

  pub fn empty(type_: impl Into<String>) -> Self {
    Self {
      type_: type_.into(),
      value: MessageValue::Empty,
    }
  }
}

/// Sink for the messages an actor emits. The host supplies the implementation
/// when it builds the [`ActorContext`], so the actor stays neighbor-ignorant:
/// it emits, and the host decides where the message goes (a downstream node's
/// transport, a state write, nowhere).
pub trait Emit: Send + Sync {
  fn emit(&self, msg: Message);
}

/// Default sink for a context with no output wired — emitted messages are
/// dropped. Used until the host attaches a real sink.
struct NoopEmit;

impl Emit for NoopEmit {
  fn emit(&self, _msg: Message) {}
}

/// Capability for an actor to schedule a delayed message to *itself*. The host
/// wires the implementation (a timer that delivers back into this actor's
/// mailbox) into the context; the actor calls `ctx.schedule_self`. This is what
/// time-based operators (debounce, throttle) use — they arm a timer on input
/// and act when it fires.
pub trait Schedule: Send + Sync {
  fn schedule_self(&self, after: Duration, msg: Message);
}

/// Default schedule for a context with no timer wired — scheduled messages
/// never fire. Used until the host attaches a real scheduler.
struct NoopSchedule;

impl Schedule for NoopSchedule {
  fn schedule_self(&self, _after: Duration, _msg: Message) {}
}

/// The per-instance capabilities a host grants an actor, **injected when the
/// actor is built** (via [`crate::ActorCreator::create`]) — not carried on the
/// per-call context.
///
/// A typed bag, keyed by each capability's trait-object type: fuchsia ships the
/// *universal* capabilities `emit` + `schedule`, and a host inserts its own
/// **domain** capabilities (a state sink, http, kv, …) under their own trait
/// types via [`insert`](Self::insert) — fuchsia never needs to know those types
/// exist. An actor pulls only what it uses, so its struct declares what it can
/// do (a debounce holds emit + schedule). These are per-instance and stable for
/// the actor's life — unlike identity, which is per-call. (Hard scoping for
/// untrusted WASM/Lua is the WIT world; for native actors this injection point
/// is hygiene, not enforcement.)
#[derive(Default)]
pub struct ActorCapabilities {
  // Keyed by `TypeId::of::<Arc<dyn Trait>>()`; the boxed value IS that `Arc`,
  // so retrieval hands back the trait object, never the concrete impl.
  map: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl ActorCapabilities {
  /// An empty bag — no capabilities granted.
  pub fn new() -> Self {
    Self::default()
  }

  /// Grant a capability. **Pass the trait-object type** (`insert::<dyn
  /// Schedule>(arc)` / coerce to `Arc<dyn Schedule>`), not the concrete impl,
  /// or a lookup by trait will miss. Prefer the `with_*` helpers for the
  /// well-known capabilities, which pin the trait type for you.
  pub fn insert<C: ?Sized + Send + Sync + 'static>(&mut self, cap: Arc<C>) {
    self.map.insert(TypeId::of::<Arc<C>>(), Box::new(cap));
  }

  /// Retrieve a granted capability by its trait type, if present.
  pub fn get<C: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<C>> {
    self
      .map
      .get(&TypeId::of::<Arc<C>>())?
      .downcast_ref::<Arc<C>>()
      .cloned()
  }

  pub fn with_emit(mut self, emit: Arc<dyn Emit>) -> Self {
    self.insert(emit);
    self
  }

  pub fn with_schedule(mut self, schedule: Arc<dyn Schedule>) -> Self {
    self.insert(schedule);
    self
  }

  /// The emit handle — falls back to a no-op sink if none was granted, so an
  /// actor can always emit (emissions just go nowhere when unwired).
  pub fn emit(&self) -> Arc<dyn Emit> {
    self.get::<dyn Emit>().unwrap_or_else(|| Arc::new(NoopEmit))
  }

  /// The scheduler handle — falls back to a no-op (scheduled messages never
  /// fire) if none was granted.
  pub fn schedule(&self) -> Arc<dyn Schedule> {
    self
      .get::<dyn Schedule>()
      .unwrap_or_else(|| Arc::new(NoopSchedule))
  }
}

/// Per-call identity threaded through every actor invocation — mirrors the WIT
/// context record (execution-id, node-id, task-id). Just *who / which run*;
/// capabilities are injected separately, at construction.
#[derive(Clone, Debug)]
pub struct ActorContext {
  pub execution_id: String,
  pub node_id: String,
  pub task_id: String,
}

impl ActorContext {
  pub fn new(
    execution_id: impl Into<String>,
    node_id: impl Into<String>,
    task_id: impl Into<String>,
  ) -> Self {
    Self {
      execution_id: execution_id.into(),
      node_id: node_id.into(),
      task_id: task_id.into(),
    }
  }
}

pub trait Actor: Send + 'static {
  fn setup(&mut self, ctx: &ActorContext) -> Result<(), ActorError>;
  fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError>;
  fn teardown(&mut self, ctx: &ActorContext) -> Result<(), ActorError>;
}
