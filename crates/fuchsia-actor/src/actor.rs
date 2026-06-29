use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

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

/// The default output port. An actor that does not select a port emits here,
/// and an edge wired without an explicit source port leaves from here.
pub const DEFAULT_PORT: &str = "out";

/// Sink for the messages an actor emits. The host supplies the implementation
/// when it builds the [`ActorContext`], so the actor stays neighbor-ignorant:
/// it emits to a *named output port*, and the host decides which successors
/// that port reaches (a downstream node's transport, a state write, nowhere).
///
/// An actor names a port (one of *its own* outputs — `"out"`, `"true"`,
/// `"case-a"`, …), never a peer; the graph still decides which actor each port
/// connects to.
pub trait Emit: Send + Sync {
  /// Emit `msg` on the named output `port`.
  fn emit_to(&self, port: &str, msg: Message);

  /// Convenience: emit on the default [`DEFAULT_PORT`] (`"out"`) port. Existing
  /// single-output actors keep calling this unchanged.
  fn emit(&self, msg: Message) {
    self.emit_to(DEFAULT_PORT, msg);
  }
}

/// Default sink for a context with no output wired — emitted messages are
/// dropped. Used until the host attaches a real sink.
struct NoopEmit;

impl Emit for NoopEmit {
  fn emit_to(&self, _port: &str, _msg: Message) {}
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
///
/// The string ids (`execution_id`, `node_id`) are `Arc<str>` rather than
/// `String` because the runtime rebuilds this context **per message of every
/// actor**, and both have a stable source it can share rather than re-allocate:
/// `execution_id` from the delivery's correlation, `node_id` from the actor's
/// stable spawn-time id. With `Arc<str>` those per-message copies are refcount
/// bumps, not allocations. This matches the principle the output-ports RFC set
/// for `Arc<str>` — worth it precisely when cloned per message.
///
/// `task_id` ("this handling") is a raw `u64` monotonic counter, not a string:
/// it's the one id with no stable source to share, so a string would be a fresh
/// allocation on every message — yet nothing in the runtime consumes it for
/// correctness (it's never compared or routed on, only surfaced into the guest
/// context tables). A `u64` is just an increment; the `"task-N"` string is
/// rendered lazily, only at the guest boundary that needs one (see
/// [`ActorContext::task_label`]), so native actors that never read it pay
/// nothing.
#[derive(Clone, Debug)]
pub struct ActorContext {
  pub execution_id: Arc<str>,
  pub node_id: Arc<str>,
  pub task_id: u64,
}

impl ActorContext {
  /// Build a context. The string ids accept anything convertible to `Arc<str>`,
  /// so callers can pass `&str` literals (tests), an owned `String`, or — the
  /// hot path — an already-shared `Arc<str>` (correlation / node id), where the
  /// conversion is a refcount bump rather than an allocation. `task_id` is the
  /// raw per-message counter (no allocation).
  pub fn new(
    execution_id: impl Into<Arc<str>>,
    node_id: impl Into<Arc<str>>,
    task_id: u64,
  ) -> Self {
    Self {
      execution_id: execution_id.into(),
      node_id: node_id.into(),
      task_id,
    }
  }

  /// The guest-visible task label: the `task_id` counter rendered as `"task-N"`.
  /// This is the *only* place the string is materialised — call it at the guest
  /// boundary (Lua/Wasm context tables) where a `String` is required anyway; the
  /// native hot path leaves `task_id` as a bare `u64`.
  pub fn task_label(&self) -> String {
    format!("task-{}", self.task_id)
  }
}

#[async_trait]
pub trait Actor: Send + 'static {
  /// Called once before the first `handle`. Defaults to a no-op, so an actor
  /// with no startup work implements only `handle`.
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError>;
  /// Called once after the last `handle`. Defaults to a no-op, so an actor with
  /// no teardown work implements only `handle`.
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  /// Records the (port, message-type) of every emission.
  struct PortRecorder(Mutex<Vec<(String, String)>>);

  impl Emit for PortRecorder {
    fn emit_to(&self, port: &str, msg: Message) {
      self
        .0
        .lock()
        .expect("lock")
        .push((port.to_owned(), msg.type_));
    }
  }

  #[test]
  fn emit_delegates_to_default_port() {
    let recorder = PortRecorder(Mutex::new(Vec::new()));
    // The default `emit` must land on `"out"`.
    recorder.emit(Message::empty("a"));
    recorder.emit_to("true", Message::empty("b"));
    let recorded = recorder.0.lock().expect("lock");
    assert_eq!(
      *recorded,
      vec![
        ("out".to_owned(), "a".to_owned()),
        ("true".to_owned(), "b".to_owned()),
      ]
    );
  }

  #[test]
  fn default_port_constant_is_out() {
    assert_eq!(DEFAULT_PORT, "out");
  }
}
