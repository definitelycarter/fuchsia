//! The `if` builtin wired through the engine: the chosen port routes; an
//! unwired port counts a `no_route` and delivers nowhere. End-to-end proof
//! that a `Fixed`-port builtin and the engine's per-port routing compose.

use std::sync::{Arc, Mutex};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  async_trait,
};
use fuchsia_actor_builtins::IfCreator;
use fuchsia_engine::{CorrelationId, Engine};
use tokio::sync::Notify;

struct Recorder {
  recorded: Arc<Mutex<Vec<Message>>>,
  notify: Arc<Notify>,
}

#[async_trait]
impl Actor for Recorder {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.recorded.lock().unwrap().push(msg);
    self.notify.notify_one();
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
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

fn if_config() -> ActorConfig {
  ActorConfig {
    settings: bson::doc! { "field": "temp", "op": "gt", "value": 30 },
    ..Default::default()
  }
}

#[tokio::test]
async fn if_routes_true_branch_and_leaves_false_unwired() {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Engine::new();
  engine.register("if", IfCreator).await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        recorded: recorded.clone(),
        notify: notify.clone(),
      },
    )
    .await;

  let hot = ActorId::new("hot?");
  let alert = ActorId::new("alert");
  engine
    .add_node(hot.clone(), "if", &if_config(), ActorCapabilities::new())
    .await
    .unwrap();
  engine
    .add_node(
      alert.clone(),
      "recorder",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Only the "true" port is wired; "false" is deliberately left unwired.
  engine.add_edge(hot.clone(), "true", alert.clone()).unwrap();

  // 42 > 30 → routes on "true" to the alert recorder.
  engine
    .push(
      &hot,
      Message::json("reading", serde_json::json!({ "temp": 42 })),
      CorrelationId::new(),
    )
    .unwrap();
  notify.notified().await;
  assert_eq!(recorded.lock().unwrap().len(), 1);
  assert_eq!(engine.route_counts(&hot, "true").unwrap().delivered, 1);

  // 20 < 30 → emits on the unwired "false" port: counted no_route, nowhere
  // delivered.
  engine
    .push(
      &hot,
      Message::json("reading", serde_json::json!({ "temp": 20 })),
      CorrelationId::new(),
    )
    .unwrap();

  let mut counts = engine.route_counts(&hot, "false").unwrap();
  for _ in 0..100 {
    if counts.no_route >= 1 {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    counts = engine.route_counts(&hot, "false").unwrap();
  }
  assert_eq!(counts.no_route, 1);
  assert_eq!(counts.delivered, 0);
  // Still only the one true-branch message reached the recorder.
  assert_eq!(recorded.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn if_edge_to_undeclared_port_is_rejected() {
  let engine = Engine::new();
  engine.register("if", IfCreator).await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        recorded: Arc::new(Mutex::new(Vec::new())),
        notify: Arc::new(Notify::new()),
      },
    )
    .await;

  let hot = ActorId::new("hot?");
  let sink = ActorId::new("sink");
  engine
    .add_node(hot.clone(), "if", &if_config(), ActorCapabilities::new())
    .await
    .unwrap();
  engine
    .add_node(
      sink.clone(),
      "recorder",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // `if` declares only true/false — "maybe" is rejected up front.
  let err = engine
    .add_edge(hot.clone(), "maybe", sink.clone())
    .unwrap_err();
  assert!(matches!(
    err,
    fuchsia_engine::EngineError::UnknownPort { .. }
  ));
}
