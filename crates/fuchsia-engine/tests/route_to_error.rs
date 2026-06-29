//! `OnError::RouteToError` (the error output port) through the engine: a node
//! whose `handle` errors under a `route_to_error` policy diverts the failure to
//! its reserved `"error"` port as an envelope, the envelope carries the
//! triggering run's correlation, an unwired `"error"` port degrades to
//! `no_route`, and a diverted error reports `Ok` on the durable ack (not a
//! retriable `Handle`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId,
  FailurePolicy, Message, MessageValue, async_trait,
};
use fuchsia_engine::CorrelationId;
use fuchsia_engine::{Engine, EngineError};
use tokio::sync::Notify;

// ---- An actor whose `handle` always errors -----------------------------------

struct ErrActor {
  calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Actor for ErrActor {
  async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    self.calls.fetch_add(1, Ordering::SeqCst);
    Err(ActorError::Handle("boom".to_owned()))
  }
}

struct ErrCreator {
  calls: Arc<AtomicUsize>,
}

impl ActorCreator for ErrCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(ErrActor {
      calls: self.calls.clone(),
    }))
  }
}

// ---- Terminal recorder: captures each message + the run it arrived on --------

struct Recorder {
  recorded: Arc<Mutex<Vec<Message>>>,
  correlations: Arc<Mutex<Vec<String>>>,
  notify: Arc<Notify>,
}

#[async_trait]
impl Actor for Recorder {
  async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    // `execution_id` is the delivery's correlation — proving the error branch
    // fired on the *triggering run*.
    self
      .correlations
      .lock()
      .unwrap()
      .push(ctx.execution_id.to_string());
    self.recorded.lock().unwrap().push(msg);
    self.notify.notify_one();
    Ok(())
  }
}

struct RecorderCreator {
  recorded: Arc<Mutex<Vec<Message>>>,
  correlations: Arc<Mutex<Vec<String>>>,
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
      correlations: self.correlations.clone(),
      notify: self.notify.clone(),
    }))
  }
}

struct RecorderHandles {
  recorded: Arc<Mutex<Vec<Message>>>,
  correlations: Arc<Mutex<Vec<String>>>,
  notify: Arc<Notify>,
}

fn recorder() -> (RecorderCreator, RecorderHandles) {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let correlations = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());
  (
    RecorderCreator {
      recorded: recorded.clone(),
      correlations: correlations.clone(),
      notify: notify.clone(),
    },
    RecorderHandles {
      recorded,
      correlations,
      notify,
    },
  )
}

fn route_to_error_config() -> ActorConfig {
  ActorConfig {
    failure: FailurePolicy::route_to_error(),
    ..Default::default()
  }
}

// ---- Tests -------------------------------------------------------------------

#[tokio::test]
async fn handled_error_routes_an_envelope_to_the_error_port() {
  let calls = Arc::new(AtomicUsize::new(0));
  let (rec_creator, rec) = recorder();

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;
  engine.register("handler", rec_creator).await;

  engine
    .add_node(
      ActorId::new("failing"),
      "failing",
      &route_to_error_config(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("handler"),
      "handler",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  // Wire the failing node's reserved "error" port to the handler.
  engine
    .add_edge(ActorId::new("failing"), "error", ActorId::new("handler"))
    .unwrap();

  engine
    .push(
      &ActorId::new("failing"),
      Message::json("reading", serde_json::json!({ "temp": 99 })),
      CorrelationId::new(),
    )
    .unwrap();
  rec.notify.notified().await;

  // Inspect the envelope in a tight block so the mutex guard is released before
  // any later `.await` (and before re-locking below).
  {
    let recorded = rec.recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let envelope = &recorded[0];
    assert_eq!(envelope.type_, "error");
    let MessageValue::Json(body) = &envelope.value else {
      panic!("envelope payload should be JSON");
    };
    assert_eq!(body["error"], "handle failed: boom");
    assert_eq!(body["node"], "failing");
    assert_eq!(body["type"], "reading");
    assert_eq!(body["payload"], serde_json::json!({ "temp": 99 }));
  }

  // The "error" port delivered exactly once; the failing node's "out" did not.
  let err_counts = engine
    .route_counts(&ActorId::new("failing"), "error")
    .unwrap();
  assert_eq!(err_counts.delivered, 1);
  assert_eq!(err_counts.no_route, 0);

  // The node continues: a second push errors again and re-routes to "error".
  // Poll (bounded) rather than reuse the edge-triggered `Notify` across awaits.
  engine
    .push(
      &ActorId::new("failing"),
      Message::empty("again"),
      CorrelationId::new(),
    )
    .unwrap();
  for _ in 0..100 {
    if rec.recorded.lock().unwrap().len() >= 2 {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
  }
  assert_eq!(rec.recorded.lock().unwrap().len(), 2);
  assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn error_envelope_carries_the_triggering_correlation() {
  let calls = Arc::new(AtomicUsize::new(0));
  let (rec_creator, rec) = recorder();

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;
  engine.register("handler", rec_creator).await;

  engine
    .add_node(
      ActorId::new("failing"),
      "failing",
      &route_to_error_config(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("handler"),
      "handler",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_edge(ActorId::new("failing"), "error", ActorId::new("handler"))
    .unwrap();

  // Push with a known run id; the error branch must see exactly it.
  let run = CorrelationId::from("run-abc");
  engine
    .push(&ActorId::new("failing"), Message::empty("go"), run.clone())
    .unwrap();
  rec.notify.notified().await;

  let correlations = rec.correlations.lock().unwrap();
  assert_eq!(correlations.len(), 1);
  assert_eq!(correlations[0], "run-abc");
}

#[tokio::test]
async fn unwired_error_port_degrades_to_no_route_and_node_survives() {
  let calls = Arc::new(AtomicUsize::new(0));

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;
  // No edge from "error" — the envelope routes nowhere.
  engine
    .add_node(
      ActorId::new("failing"),
      "failing",
      &route_to_error_config(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  engine
    .push(
      &ActorId::new("failing"),
      Message::empty("go"),
      CorrelationId::new(),
    )
    .unwrap();

  // Poll the "error" port's counters until the no-route is recorded (the node
  // ran and emitted), bounded so a regression fails fast.
  let mut counts = engine
    .route_counts(&ActorId::new("failing"), "error")
    .unwrap();
  for _ in 0..100 {
    if counts.no_route >= 1 {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    counts = engine
      .route_counts(&ActorId::new("failing"), "error")
      .unwrap();
  }
  assert_eq!(
    counts.no_route, 1,
    "an unwired error port must count one no_route on (node, \"error\")"
  );
  assert_eq!(counts.delivered, 0);

  // The node survived the diverted-to-nowhere error: a second message is still
  // handled (the run loop did not stop).
  engine
    .push(
      &ActorId::new("failing"),
      Message::empty("again"),
      CorrelationId::new(),
    )
    .unwrap();
  for _ in 0..100 {
    if calls.load(Ordering::SeqCst) >= 2 {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
  }
  assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn push_durable_reports_ok_for_a_diverted_error() {
  // The central design decision: a `route_to_error` node whose handle errors is
  // *diverted, not retriable*, so the durable ack reports `Ok` — not
  // `EngineError::Handle`. Reporting `Err` would make an at-least-once caller
  // retry and double-fire the error branch.
  let calls = Arc::new(AtomicUsize::new(0));
  let (rec_creator, _rec) = recorder();

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;
  engine.register("handler", rec_creator).await;

  engine
    .add_node(
      ActorId::new("failing"),
      "failing",
      &route_to_error_config(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("handler"),
      "handler",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_edge(ActorId::new("failing"), "error", ActorId::new("handler"))
    .unwrap();

  // Despite the handler erroring, push_durable resolves Ok (diverted).
  engine
    .push_durable(
      &ActorId::new("failing"),
      Message::empty("job"),
      CorrelationId::new(),
    )
    .await
    .unwrap();
  assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// Sanity: without `route_to_error` (the default `continue`), `push_durable`
// still surfaces the handler error as `EngineError::Handle` — guarding that the
// `Ok` above is specific to the diversion, not a regression of the durable path.
#[tokio::test]
async fn push_durable_without_route_to_error_still_surfaces_handle_error() {
  let calls = Arc::new(AtomicUsize::new(0));

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;
  engine
    .add_node(
      ActorId::new("failing"),
      "failing",
      &ActorConfig::default(), // default = OnError::Continue
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  let err = engine
    .push_durable(
      &ActorId::new("failing"),
      Message::empty("job"),
      CorrelationId::new(),
    )
    .await
    .unwrap_err();
  assert!(matches!(err, EngineError::Handle(ActorError::Handle(_))));
}
