//! Closure node types (`FnCreator` / the `register_fn` mechanism) wired through
//! the engine: end-to-end proof that an actor spelled as just a closure is
//! instantiated, routed, and validated on the same path as a hand-written one —
//! including the stateful `from_fn_with_state` adapter and `Fixed`-port edge
//! validation.

use std::sync::{Arc, Mutex};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId,
  FnCreator, Message, async_trait, from_fn, from_fn_with_state,
};
use fuchsia_engine::{CorrelationId, Engine};
use tokio::sync::Notify;

/// A recorder sink node, so a test can observe what routed downstream.
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

#[tokio::test]
async fn stateful_closure_node_routes_through_the_engine() {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Engine::new();
  // A `counter` node type defined as just a closure over `from_fn_with_state`:
  // it counts inputs and emits the running total. Registered through the engine
  // as a plain `ActorCreator` — exactly what `ActorFactory::register_fn` builds.
  engine
    .register(
      "counter",
      FnCreator::new(|_config, caps| {
        from_fn_with_state(0u64, caps.emit(), |count, _ctx, _msg, emit| {
          *count += 1;
          let total = *count;
          async move {
            emit.emit(Message::json("count", serde_json::json!(total)));
            Ok(())
          }
        })
      }),
    )
    .await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        recorded: recorded.clone(),
        notify: notify.clone(),
      },
    )
    .await;

  let counter = ActorId::new("counter");
  let sink = ActorId::new("sink");
  engine
    .add_node(
      counter.clone(),
      "counter",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
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
  engine
    .add_default_edge(counter.clone(), sink.clone())
    .unwrap();

  // Two inputs → two emissions on "out", routed to the recorder, carrying the
  // accumulated state (1 then 2).
  engine
    .push(&counter, Message::empty("tick"), CorrelationId::new())
    .unwrap();
  notify.notified().await;
  engine
    .push(&counter, Message::empty("tick"), CorrelationId::new())
    .unwrap();
  notify.notified().await;

  let out = recorded.lock().unwrap();
  assert_eq!(out.len(), 2);
  assert_eq!(
    out[0].value,
    fuchsia_actor::MessageValue::Json(serde_json::json!(1))
  );
  assert_eq!(
    out[1].value,
    fuchsia_actor::MessageValue::Json(serde_json::json!(2))
  );
  assert_eq!(engine.route_counts(&counter, "out").unwrap().delivered, 2);
}

#[tokio::test]
async fn fixed_port_closure_node_routes_and_validates_edges() {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Engine::new();
  // A closure node declaring a `Fixed` interface — `["left", "right"]` — so the
  // engine validates edges against it. The body emits on "left".
  engine
    .register(
      "split",
      FnCreator::with_ports(
        vec!["left".to_owned(), "right".to_owned()],
        |_config, caps| {
          from_fn(caps.emit(), |_ctx, msg, emit| async move {
            emit.emit_to("left", msg);
            Ok(())
          })
        },
      ),
    )
    .await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        recorded: recorded.clone(),
        notify: notify.clone(),
      },
    )
    .await;

  let split = ActorId::new("split");
  let sink = ActorId::new("sink");
  engine
    .add_node(
      split.clone(),
      "split",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
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

  // An edge to a port the node never declared is rejected up front.
  let err = engine
    .add_edge(split.clone(), "middle", sink.clone())
    .unwrap_err();
  assert!(matches!(
    err,
    fuchsia_engine::EngineError::UnknownPort { .. }
  ));

  // The declared "left" port wires and routes.
  engine
    .add_edge(split.clone(), "left", sink.clone())
    .unwrap();
  engine
    .push(&split, Message::empty("x"), CorrelationId::new())
    .unwrap();
  notify.notified().await;
  assert_eq!(recorded.lock().unwrap().len(), 1);
  assert_eq!(engine.route_counts(&split, "left").unwrap().delivered, 1);
}
