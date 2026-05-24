use async_trait::async_trait;
use fuchsia_actor::{Actor, ActorError, Context, Emitter, Inbox, Message, MessageValue};
use fuchsia_runtime::{ActorRegistry, Edge, Graph, Node, Orchestrator};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---- Test actors --------------------------------------------------------

struct Passthrough;

#[async_trait]
impl Actor for Passthrough {
  async fn run(&self, mut inbox: Inbox, emit: Emitter, ctx: Context) -> Result<(), ActorError> {
    loop {
      tokio::select! {
          _ = ctx.cancelled() => return Ok(()),
          msg = inbox.recv() => match msg {
              Some(msg) => emit.send(msg).await?,
              None => return Ok(()),
          }
      }
    }
  }
}

struct Doubler;

#[async_trait]
impl Actor for Doubler {
  async fn run(&self, mut inbox: Inbox, emit: Emitter, ctx: Context) -> Result<(), ActorError> {
    loop {
      tokio::select! {
          _ = ctx.cancelled() => return Ok(()),
          msg = inbox.recv() => match msg {
              Some(msg) => {
                  if let MessageValue::Json(Value::Number(n)) = &msg.value {
                      let d = n.as_f64().unwrap_or(0.0) * 2.0;
                      emit.send(Message::json("doubled", json!(d))).await?;
                  }
              }
              None => return Ok(()),
          }
      }
    }
  }
}

struct Recorder {
  out: Arc<Mutex<Vec<Message>>>,
}

#[async_trait]
impl Actor for Recorder {
  async fn run(&self, mut inbox: Inbox, _emit: Emitter, ctx: Context) -> Result<(), ActorError> {
    loop {
      tokio::select! {
          _ = ctx.cancelled() => return Ok(()),
          msg = inbox.recv() => match msg {
              Some(msg) => self.out.lock().unwrap().push(msg),
              None => return Ok(()),
          }
      }
    }
  }
}

struct Debouncer {
  window: Duration,
}

#[async_trait]
impl Actor for Debouncer {
  async fn run(&self, mut inbox: Inbox, emit: Emitter, ctx: Context) -> Result<(), ActorError> {
    let mut pending: Option<Message> = None;
    let mut timer: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;

    loop {
      tokio::select! {
          _ = ctx.cancelled() => return Ok(()),
          msg = inbox.recv() => match msg {
              Some(msg) => {
                  pending = Some(msg);
                  timer = Some(Box::pin(tokio::time::sleep(self.window)));
              }
              None => {
                  if let Some(msg) = pending.take() {
                      emit.send(msg).await?;
                  }
                  return Ok(());
              }
          },
          _ = async { timer.as_mut().unwrap().await }, if timer.is_some() => {
              if let Some(msg) = pending.take() {
                  emit.send(msg).await?;
              }
              timer = None;
          }
      }
    }
  }
}

#[derive(Deserialize)]
struct DebouncerConfig {
  window_ms: u64,
}

// ---- Helpers ------------------------------------------------------------

fn build_registry(out: Arc<Mutex<Vec<Message>>>) -> ActorRegistry {
  let mut reg = ActorRegistry::new();
  reg.register::<Passthrough, Value, _>("passthrough", |_| Passthrough);
  reg.register::<Doubler, Value, _>("doubler", |_| Doubler);
  reg.register::<Debouncer, DebouncerConfig, _>("debounce", |cfg: DebouncerConfig| Debouncer {
    window: Duration::from_millis(cfg.window_ms),
  });
  reg.register::<Recorder, Value, _>("recorder", move |_| Recorder { out: out.clone() });
  reg
}

fn node(id: &str, actor: &str, config: Value) -> Node {
  Node {
    id: id.into(),
    actor: actor.into(),
    config,
  }
}

fn edge(from: &str, to: &str) -> Edge {
  Edge {
    from: from.into(),
    to: to.into(),
  }
}

fn assert_all_ok(results: &[Result<(), ActorError>]) {
  for (i, r) in results.iter().enumerate() {
    assert!(r.is_ok(), "actor {i} failed: {r:?}");
  }
}

// ---- Tests --------------------------------------------------------------

#[tokio::test]
async fn passthrough_smoke() {
  let out = Arc::new(Mutex::new(Vec::new()));
  let registry = build_registry(out.clone());

  let graph = Graph {
    entry: "in".into(),
    nodes: vec![
      node("in", "passthrough", Value::Null),
      node("rec", "recorder", Value::Null),
    ],
    edges: vec![edge("in", "rec")],
  };

  let orchestrator = Orchestrator::new(Arc::new(registry));
  let handle = orchestrator.start(&graph).unwrap();

  handle.send(Message::json("test", json!(42))).await.unwrap();
  handle
    .send(Message::json("test", json!("hello")))
    .await
    .unwrap();

  let results = handle.join().await;
  assert_all_ok(&results);

  let recorded = out.lock().unwrap();
  assert_eq!(recorded.len(), 2);
  assert!(matches!(&recorded[0].value, MessageValue::Json(v) if *v == json!(42)));
  assert!(matches!(&recorded[1].value, MessageValue::Json(v) if *v == json!("hello")));
}

#[tokio::test]
async fn transform_chain() {
  let out = Arc::new(Mutex::new(Vec::new()));
  let registry = build_registry(out.clone());

  let graph = Graph {
    entry: "a".into(),
    nodes: vec![
      node("a", "doubler", Value::Null),
      node("b", "doubler", Value::Null),
      node("rec", "recorder", Value::Null),
    ],
    edges: vec![edge("a", "b"), edge("b", "rec")],
  };

  let orchestrator = Orchestrator::new(Arc::new(registry));
  let handle = orchestrator.start(&graph).unwrap();

  handle.send(Message::json("test", json!(5))).await.unwrap();
  handle.send(Message::json("test", json!(3))).await.unwrap();

  let results = handle.join().await;
  assert_all_ok(&results);

  let recorded = out.lock().unwrap();
  assert_eq!(recorded.len(), 2);
  assert!(matches!(&recorded[0].value, MessageValue::Json(v) if *v == json!(20.0)));
  assert!(matches!(&recorded[1].value, MessageValue::Json(v) if *v == json!(12.0)));
}

#[tokio::test]
async fn fan_out() {
  let out_a = Arc::new(Mutex::new(Vec::new()));
  let out_b = Arc::new(Mutex::new(Vec::new()));

  let mut registry = ActorRegistry::new();
  registry.register::<Doubler, Value, _>("doubler", |_| Doubler);
  {
    let a = out_a.clone();
    registry.register::<Recorder, Value, _>("rec_a", move |_| Recorder { out: a.clone() });
  }
  {
    let b = out_b.clone();
    registry.register::<Recorder, Value, _>("rec_b", move |_| Recorder { out: b.clone() });
  }

  let graph = Graph {
    entry: "in".into(),
    nodes: vec![
      node("in", "doubler", Value::Null),
      node("a", "rec_a", Value::Null),
      node("b", "rec_b", Value::Null),
    ],
    edges: vec![edge("in", "a"), edge("in", "b")],
  };

  let orchestrator = Orchestrator::new(Arc::new(registry));
  let handle = orchestrator.start(&graph).unwrap();

  handle.send(Message::json("test", json!(7))).await.unwrap();

  let results = handle.join().await;
  assert_all_ok(&results);

  assert!(matches!(&out_a.lock().unwrap()[0].value, MessageValue::Json(v) if *v == json!(14.0)));
  assert!(matches!(&out_b.lock().unwrap()[0].value, MessageValue::Json(v) if *v == json!(14.0)));
}

#[tokio::test]
async fn fan_in_merge() {
  let out = Arc::new(Mutex::new(Vec::new()));
  let registry = build_registry(out.clone());

  let graph = Graph {
    entry: "in".into(),
    nodes: vec![
      node("in", "passthrough", Value::Null),
      node("left", "passthrough", Value::Null),
      node("right", "passthrough", Value::Null),
      node("rec", "recorder", Value::Null),
    ],
    edges: vec![
      edge("in", "left"),
      edge("in", "right"),
      edge("left", "rec"),
      edge("right", "rec"),
    ],
  };

  let orchestrator = Orchestrator::new(Arc::new(registry));
  let handle = orchestrator.start(&graph).unwrap();

  handle.send(Message::json("test", json!(1))).await.unwrap();
  handle.send(Message::json("test", json!(2))).await.unwrap();

  let results = handle.join().await;
  assert_all_ok(&results);

  let recorded = out.lock().unwrap();
  assert_eq!(recorded.len(), 4, "got {recorded:?}");
  let c1 = recorded
    .iter()
    .filter(|m| matches!(&m.value, MessageValue::Json(v) if *v == json!(1)))
    .count();
  let c2 = recorded
    .iter()
    .filter(|m| matches!(&m.value, MessageValue::Json(v) if *v == json!(2)))
    .count();
  assert_eq!(c1, 2);
  assert_eq!(c2, 2);
}

#[tokio::test]
async fn debounce_collapses_burst() {
  let out = Arc::new(Mutex::new(Vec::new()));
  let registry = build_registry(out.clone());

  let graph = Graph {
    entry: "deb".into(),
    nodes: vec![
      node("deb", "debounce", json!({ "window_ms": 50 })),
      node("rec", "recorder", Value::Null),
    ],
    edges: vec![edge("deb", "rec")],
  };

  let orchestrator = Orchestrator::new(Arc::new(registry));
  let handle = orchestrator.start(&graph).unwrap();

  handle.send(Message::json("test", json!(1))).await.unwrap();
  handle.send(Message::json("test", json!(2))).await.unwrap();
  handle.send(Message::json("test", json!(3))).await.unwrap();
  tokio::time::sleep(Duration::from_millis(120)).await;

  handle.send(Message::json("test", json!(99))).await.unwrap();
  tokio::time::sleep(Duration::from_millis(120)).await;

  let results = handle.join().await;
  assert_all_ok(&results);

  let recorded = out.lock().unwrap();
  assert_eq!(recorded.len(), 2);
  assert!(matches!(&recorded[0].value, MessageValue::Json(v) if *v == json!(3)));
  assert!(matches!(&recorded[1].value, MessageValue::Json(v) if *v == json!(99)));
}

#[tokio::test]
async fn cancellation_exits_cleanly() {
  let out = Arc::new(Mutex::new(Vec::new()));
  let registry = build_registry(out.clone());

  let graph = Graph {
    entry: "in".into(),
    nodes: vec![
      node("in", "passthrough", Value::Null),
      node("rec", "recorder", Value::Null),
    ],
    edges: vec![edge("in", "rec")],
  };

  let orchestrator = Orchestrator::new(Arc::new(registry));
  let handle = orchestrator.start(&graph).unwrap();

  handle.send(Message::json("test", json!(1))).await.unwrap();
  tokio::time::sleep(Duration::from_millis(20)).await;
  handle.cancel();

  let results = handle.join().await;
  assert_all_ok(&results);
}

#[tokio::test]
async fn unknown_actor_is_reported() {
  let registry = ActorRegistry::new();
  let graph = Graph {
    entry: "x".into(),
    nodes: vec![node("x", "does-not-exist", Value::Null)],
    edges: vec![],
  };
  let orchestrator = Orchestrator::new(Arc::new(registry));
  match orchestrator.start(&graph) {
    Err(ActorError::UnknownActor(_)) => {}
    Err(e) => panic!("expected UnknownActor, got {e:?}"),
    Ok(_) => panic!("expected error, got Ok"),
  }
}
