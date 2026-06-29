//! Death detection through the engine: a node whose `handle` panics must stop
//! resolving as a routable target (so upstream emits no longer silently shed
//! into a dead mailbox) and its death must be observable on `Health`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  async_trait,
};
use fuchsia_actor_builtins::PassthroughCreator;
use fuchsia_engine::{CorrelationId, Engine};
use tokio::sync::Notify;

/// Panics the first time it handles a message — the zombie-maker.
struct PanicActor;

#[async_trait]
impl Actor for PanicActor {
  async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    panic!("intentional panic in handle")
  }
}

struct PanicCreator;

impl ActorCreator for PanicCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(PanicActor))
  }
}

/// Records every message it handles and signals — a terminal sink.
struct Recorder {
  recorded: Arc<Mutex<Vec<Message>>>,
  notify: Arc<Notify>,
}

#[async_trait]
impl Actor for Recorder {
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.recorded.lock().unwrap().push(msg);
    self.notify.notify_one();
    Ok(())
  }
}

struct RecorderCreator {
  recorded: Arc<Mutex<Vec<Message>>>,
  notify: Arc<Notify>,
}

impl ActorCreator for RecorderCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Recorder {
      recorded: self.recorded.clone(),
      notify: self.notify.clone(),
    }))
  }
}

/// A panicking node, pushed a message directly, dies and stops resolving in the
/// router: a later push to it errors as `NotFound`, and its death is on `Health`.
#[tokio::test]
async fn panicking_node_stops_resolving_and_death_is_observable() {
  let engine = Engine::new();
  engine.register("panic", PanicCreator).await;
  engine
    .add_node(
      ActorId::new("p"),
      "panic",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Push a message; handling it panics, killing the node's task.
  engine
    .push(
      &ActorId::new("p"),
      Message::empty("boom"),
      CorrelationId::new(),
    )
    .unwrap();

  // Wait for the supervisor to detect the death and deregister the node from the
  // router. Poll rather than guess a fixed delay.
  let mut deregistered = false;
  for _ in 0..200 {
    if engine
      .push(
        &ActorId::new("p"),
        Message::empty("again"),
        CorrelationId::new(),
      )
      .is_err()
    {
      deregistered = true;
      break;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  assert!(
    deregistered,
    "the dead node should stop resolving as a routable target"
  );

  let err = engine
    .push(
      &ActorId::new("p"),
      Message::empty("again"),
      CorrelationId::new(),
    )
    .unwrap_err();
  assert!(matches!(err, fuchsia_engine::EngineError::NotFound(_)));
}

/// An upstream that routes to a node which then dies: once the node is
/// deregistered, the upstream's emit on that edge reads as `no_route` (the dead
/// target resolves to nothing), not a silent offer into a permanently dead
/// mailbox.
#[tokio::test]
async fn emit_to_a_dead_node_reads_as_no_route() {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Engine::new();
  engine.register("passthrough", PassthroughCreator).await;
  engine.register("panic", PanicCreator).await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        recorded: recorded.clone(),
        notify: notify.clone(),
      },
    )
    .await;

  // up (passthrough) -> dead (panics). `up` also forwards to `sink` so we have a
  // signal that `up`'s emit ran.
  engine
    .add_node(
      ActorId::new("up"),
      "passthrough",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("dead"),
      "panic",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("sink"),
      "recorder",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_default_edge(ActorId::new("up"), ActorId::new("dead"))
    .unwrap();
  engine
    .add_default_edge(ActorId::new("up"), ActorId::new("sink"))
    .unwrap();

  // First push: `up` forwards to `dead` (which panics) and to `sink`.
  engine
    .push(
      &ActorId::new("up"),
      Message::empty("first"),
      CorrelationId::new(),
    )
    .unwrap();
  notify.notified().await;

  // Wait for `dead` to be deregistered (push to it errors once it's gone).
  let mut gone = false;
  for _ in 0..200 {
    if engine
      .push(
        &ActorId::new("dead"),
        Message::empty("probe"),
        CorrelationId::new(),
      )
      .is_err()
    {
      gone = true;
      break;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  assert!(gone, "the panicking node should be deregistered");

  // Second push through `up`: the edge to `dead` now resolves to nothing, so it
  // is counted as a no-route on `(up, out)` rather than offered/shed into a dead
  // mailbox. `sink` still receives.
  let before = engine
    .route_counts(&ActorId::new("up"), fuchsia_actor::DEFAULT_PORT)
    .unwrap();
  engine
    .push(
      &ActorId::new("up"),
      Message::empty("second"),
      CorrelationId::new(),
    )
    .unwrap();
  notify.notified().await;

  let after = engine
    .route_counts(&ActorId::new("up"), fuchsia_actor::DEFAULT_PORT)
    .unwrap();
  // The dead edge contributed a no_route on the second push; sink contributed a
  // delivered.
  assert_eq!(after.no_route, before.no_route + 1);
  assert_eq!(after.delivered, before.delivered + 1);
  assert_eq!(recorded.lock().unwrap().len(), 2);
}
