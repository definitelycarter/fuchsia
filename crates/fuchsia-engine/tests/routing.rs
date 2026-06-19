use std::sync::{Arc, Mutex};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
};
use fuchsia_actor_builtins::PassthroughCreator;
use fuchsia_engine::Engine;
use tokio::sync::Notify;

/// Terminal test actor: records every message it handles and signals.
struct Recorder {
  recorded: Arc<Mutex<Vec<Message>>>,
  notify: Arc<Notify>,
}

impl Actor for Recorder {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.recorded.lock().unwrap().push(msg);
    self.notify.notify_one();
    Ok(())
  }
  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
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

#[tokio::test]
async fn message_routes_through_two_nodes() {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Engine::new();
  engine.register("passthrough", PassthroughCreator).await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        recorded: recorded.clone(),
        notify: notify.clone(),
      },
    )
    .await;

  // a (passthrough, re-emits its input) → b (recorder)
  engine
    .add_node(
      ActorId::new("a"),
      "passthrough",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("b"),
      "recorder",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_edge(ActorId::new("a"), ActorId::new("b"))
    .unwrap();

  engine
    .push(&ActorId::new("a"), Message::empty("ping"))
    .unwrap();

  notify.notified().await;

  let recorded = recorded.lock().unwrap();
  assert_eq!(recorded.len(), 1);
  assert_eq!(recorded[0].type_, "ping");
}

#[tokio::test]
async fn engine_is_shareable_across_tasks() {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Arc::new(Engine::new());
  engine.register("passthrough", PassthroughCreator).await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        recorded: recorded.clone(),
        notify: notify.clone(),
      },
    )
    .await;
  engine
    .add_node(
      ActorId::new("a"),
      "passthrough",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("b"),
      "recorder",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_edge(ActorId::new("a"), ActorId::new("b"))
    .unwrap();

  // A clone of the Arc pushes from a separate task — proves Engine is Send+Sync.
  let pusher = engine.clone();
  tokio::spawn(async move {
    pusher
      .push(&ActorId::new("a"), Message::empty("ping"))
      .unwrap();
  })
  .await
  .unwrap();

  notify.notified().await;
  assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn entrypoint_with_no_edges_is_a_dead_end() {
  let engine = Engine::new();
  engine.register("passthrough", PassthroughCreator).await;
  engine
    .add_node(
      ActorId::new("solo"),
      "passthrough",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // No edges; pushing succeeds and the emission goes nowhere.
  engine
    .push(&ActorId::new("solo"), Message::empty("ping"))
    .unwrap();
}

#[tokio::test]
async fn push_to_unknown_node_errors() {
  let engine = Engine::new();
  let err = engine
    .push(&ActorId::new("missing"), Message::empty("ping"))
    .unwrap_err();
  assert!(matches!(err, fuchsia_engine::EngineError::NotFound(_)));
}
