use std::sync::{Arc, Mutex};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  async_trait,
};
use fuchsia_actor_builtins::PassthroughCreator;
use fuchsia_engine::CorrelationId;
use fuchsia_engine::{Engine, EngineError};
use tokio::sync::Notify;

/// Records (node id, message type) for every message it handles, so a test can
/// tell *which* instance of a same-named node received what.
struct NodeRecorder {
  sink: Arc<Mutex<Vec<(String, String)>>>,
  notify: Arc<Notify>,
}

#[async_trait]
impl Actor for NodeRecorder {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self
      .sink
      .lock()
      .unwrap()
      .push((ctx.node_id.clone(), msg.type_.clone()));
    self.notify.notify_one();
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct NodeRecorderCreator {
  sink: Arc<Mutex<Vec<(String, String)>>>,
  notify: Arc<Notify>,
}

impl ActorCreator for NodeRecorderCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(NodeRecorder {
      sink: self.sink.clone(),
      notify: self.notify.clone(),
    }))
  }
}

#[tokio::test]
async fn groups_isolate_and_tear_down_independently() {
  let sink = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Engine::new();
  engine.register("passthrough", PassthroughCreator).await;
  engine
    .register(
      "recorder",
      NodeRecorderCreator {
        sink: sink.clone(),
        notify: notify.clone(),
      },
    )
    .await;

  // Two graphs with the SAME local node ids ("a", "b") — the group keeps them
  // from colliding. Each: a (passthrough) → b (recorder).
  for group in ["g1", "g2"] {
    engine
      .add_node(
        ActorId::scoped(group, "a"),
        "passthrough",
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
    engine
      .add_node(
        ActorId::scoped(group, "b"),
        "recorder",
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
    engine
      .add_default_edge(ActorId::scoped(group, "a"), ActorId::scoped(group, "b"))
      .unwrap();
  }

  // Push into g1 only; it must land on g1's recorder, not g2's.
  engine
    .push(
      &ActorId::scoped("g1", "a"),
      Message::empty("ping"),
      CorrelationId::new(),
    )
    .unwrap();
  notify.notified().await;
  {
    let s = sink.lock().unwrap();
    assert_eq!(s.as_slice(), [("g1/b".to_string(), "ping".to_string())]);
  }

  // Tear g1 down; g2 is untouched.
  engine.remove_graph("g1").await.unwrap();

  // g1's entrypoint is gone.
  assert!(matches!(
    engine
      .push(
        &ActorId::scoped("g1", "a"),
        Message::empty("x"),
        CorrelationId::new()
      )
      .unwrap_err(),
    EngineError::NotFound(_)
  ));

  // g2 still routes after g1's teardown.
  engine
    .push(
      &ActorId::scoped("g2", "a"),
      Message::empty("pong"),
      CorrelationId::new(),
    )
    .unwrap();
  notify.notified().await;
  {
    let s = sink.lock().unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[1], ("g2/b".to_string(), "pong".to_string()));
  }
}
