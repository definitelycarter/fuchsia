//! Named-output-port routing through the engine: per-port edges, edge
//! validation against a node's declared ports, and the route-outcome counters.

use std::sync::{Arc, Mutex};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Emit,
  Message, OutputPorts, async_trait,
};
use fuchsia_engine::{Engine, EngineError};
use tokio::sync::Notify;

// ---- A source that emits each input on a port named in its `type_` ---------
//
// On `handle(msg)` it emits `msg` on the port `msg.type_` — so a test can drive
// "emit on `true`" / "emit on `nope`" by pushing a message whose type is that
// port name. Declares a `Fixed` port set, configurable per instance, so edge
// validation can be exercised.

struct PortSource {
  emit: Arc<dyn Emit>,
}

#[async_trait]
impl Actor for PortSource {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let port = msg.type_.clone();
    self.emit.emit_to(&port, msg);
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

/// A `Fixed`-port source, advertising the given ports.
struct FixedSourceCreator {
  ports: Vec<String>,
}

impl ActorCreator for FixedSourceCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(PortSource { emit: caps.emit() }))
  }
  fn output_ports(&self, _config: &ActorConfig) -> OutputPorts {
    OutputPorts::Fixed(self.ports.clone())
  }
}

/// A `Dynamic`-port source (the default `output_ports`), same emit behavior.
struct DynamicSourceCreator;

impl ActorCreator for DynamicSourceCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(PortSource { emit: caps.emit() }))
  }
}

// ---- Terminal recorder ------------------------------------------------------

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

fn recorder() -> (RecorderCreator, Arc<Mutex<Vec<Message>>>, Arc<Notify>) {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());
  (
    RecorderCreator {
      recorded: recorded.clone(),
      notify: notify.clone(),
    },
    recorded,
    notify,
  )
}

// ---- Tests ------------------------------------------------------------------

#[tokio::test]
async fn emission_routes_only_to_the_matching_port() {
  let (t_creator, t_recorded, t_notify) = recorder();
  let (f_creator, f_recorded, _f_notify) = recorder();

  let engine = Engine::new();
  engine
    .register(
      "src",
      FixedSourceCreator {
        ports: vec!["true".to_owned(), "false".to_owned()],
      },
    )
    .await;
  engine.register("on_true", t_creator).await;
  engine.register("on_false", f_creator).await;

  for (id, ty) in [("src", "src"), ("t", "on_true"), ("f", "on_false")] {
    engine
      .add_node(
        ActorId::new(id),
        ty,
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
  }
  engine
    .add_edge(ActorId::new("src"), "true", ActorId::new("t"))
    .unwrap();
  engine
    .add_edge(ActorId::new("src"), "false", ActorId::new("f"))
    .unwrap();

  // Push a message of type "true": the source emits it on the "true" port.
  engine
    .push(&ActorId::new("src"), Message::empty("true"))
    .unwrap();
  t_notify.notified().await;

  assert_eq!(t_recorded.lock().unwrap().len(), 1);
  assert!(f_recorded.lock().unwrap().is_empty());

  // The "true" port delivered exactly one message.
  let counts = engine.route_counts(&ActorId::new("src"), "true").unwrap();
  assert_eq!(counts.delivered, 1);
  assert_eq!(counts.no_route, 0);
}

#[tokio::test]
async fn add_edge_rejects_an_undeclared_port_on_a_fixed_node() {
  let engine = Engine::new();
  engine
    .register(
      "src",
      FixedSourceCreator {
        ports: vec!["true".to_owned(), "false".to_owned()],
      },
    )
    .await;
  let (rec, _r, _n) = recorder();
  engine.register("rec", rec).await;

  for (id, ty) in [("src", "src"), ("rec", "rec")] {
    engine
      .add_node(
        ActorId::new(id),
        ty,
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
  }

  // "nope" is not among the source's declared ports.
  let err = engine
    .add_edge(ActorId::new("src"), "nope", ActorId::new("rec"))
    .unwrap_err();
  assert!(
    matches!(err, EngineError::UnknownPort { ref node, ref port } if node == &ActorId::new("src") && port == "nope")
  );

  // A declared port wires fine.
  assert!(
    engine
      .add_edge(ActorId::new("src"), "true", ActorId::new("rec"))
      .is_ok()
  );
}

#[tokio::test]
async fn out_and_error_ports_are_always_allowed_on_a_fixed_node() {
  let engine = Engine::new();
  engine
    .register(
      "src",
      FixedSourceCreator {
        // Declares neither "out" nor "error".
        ports: vec!["true".to_owned()],
      },
    )
    .await;
  let (rec, _r, _n) = recorder();
  engine.register("rec", rec).await;

  for (id, ty) in [("src", "src"), ("rec", "rec")] {
    engine
      .add_node(
        ActorId::new(id),
        ty,
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
  }

  // "out" is always allowed; "error" is reserved for the failure branch.
  assert!(
    engine
      .add_edge(ActorId::new("src"), "out", ActorId::new("rec"))
      .is_ok()
  );
  assert!(
    engine
      .add_edge(ActorId::new("src"), "error", ActorId::new("rec"))
      .is_ok()
  );
}

#[tokio::test]
async fn dynamic_node_accepts_any_port() {
  let engine = Engine::new();
  engine.register("src", DynamicSourceCreator).await;
  let (rec, _r, _n) = recorder();
  engine.register("rec", rec).await;

  for (id, ty) in [("src", "src"), ("rec", "rec")] {
    engine
      .add_node(
        ActorId::new(id),
        ty,
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
  }

  // A Dynamic source's ports exist only at emit time — any port wires.
  assert!(
    engine
      .add_edge(ActorId::new("src"), "whatever", ActorId::new("rec"))
      .is_ok()
  );
}

#[tokio::test]
async fn emitting_on_an_unwired_port_counts_no_route_and_delivers_nowhere() {
  let (rec, recorded, _notify) = recorder();

  let engine = Engine::new();
  engine.register("src", DynamicSourceCreator).await;
  engine.register("rec", rec).await;

  for (id, ty) in [("src", "src"), ("rec", "rec")] {
    engine
      .add_node(
        ActorId::new(id),
        ty,
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
  }
  // Wire only the "wired" port; "lonely" stays unwired.
  engine
    .add_edge(ActorId::new("src"), "wired", ActorId::new("rec"))
    .unwrap();

  // Emit on the unwired "lonely" port. It was neither declared (the source is
  // Dynamic) nor wired, so its no-route lands on the node's per-node *fallback*
  // bucket — counted, never silent — not under the exact "lonely" port name.
  engine
    .push(&ActorId::new("src"), Message::empty("lonely"))
    .unwrap();

  // Give the source's task time to handle and emit. Poll the fallback until the
  // no-route is recorded (the source ran), bounded so a regression fails fast.
  let mut fallback = engine.route_counts_fallback(&ActorId::new("src")).unwrap();
  for _ in 0..100 {
    if fallback.no_route >= 1 {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    fallback = engine.route_counts_fallback(&ActorId::new("src")).unwrap();
  }

  assert_eq!(
    fallback.no_route, 1,
    "an unwired, never-declared port must count one no-route on the fallback"
  );
  assert_eq!(fallback.delivered, 0);
  // The exact port name has no per-port counter — it reads zero.
  let by_name = engine.route_counts(&ActorId::new("src"), "lonely").unwrap();
  assert_eq!(by_name, fuchsia_engine::RouteCounts::default());
  // Nothing was delivered anywhere.
  assert!(recorded.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_port_fans_out_to_several_edges() {
  let (a_creator, a_recorded, a_notify) = recorder();
  let (b_creator, b_recorded, b_notify) = recorder();

  let engine = Engine::new();
  engine.register("src", DynamicSourceCreator).await;
  engine.register("a", a_creator).await;
  engine.register("b", b_creator).await;

  for (id, ty) in [("src", "src"), ("a", "a"), ("b", "b")] {
    engine
      .add_node(
        ActorId::new(id),
        ty,
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
  }
  // One port, two edges — fan-out within a port is preserved.
  engine
    .add_edge(ActorId::new("src"), "out", ActorId::new("a"))
    .unwrap();
  engine
    .add_edge(ActorId::new("src"), "out", ActorId::new("b"))
    .unwrap();

  engine
    .push(&ActorId::new("src"), Message::empty("out"))
    .unwrap();
  a_notify.notified().await;
  b_notify.notified().await;

  assert_eq!(a_recorded.lock().unwrap().len(), 1);
  assert_eq!(b_recorded.lock().unwrap().len(), 1);
  let counts = engine.route_counts(&ActorId::new("src"), "out").unwrap();
  assert_eq!(counts.delivered, 2);
}
