//! Proves a trace follows a message across the mailbox/task boundary, *and*
//! that the run's correlation rides every span as a first-class field — plus the
//! control-plane (topology) spans on graph mutation.
//!
//! The data-plane span chain for a one-hop graph `a → b`, triggered inside a
//! root span, is:
//!
//! ```text
//!   ingress                 (the host's root span at the push site)
//!   └─ run                  (fuchsia-engine: Engine::push, keyed on correlation)
//!      └─ actor.handle (a)  (fuchsia-runtime, parented by the delivery's span)
//!         └─ engine.route   (fuchsia-engine: the fan-out a → b)
//!            └─ actor.handle (b)
//! ```
//!
//! That parent chain is what `#[instrument]` alone can't produce (each actor
//! runs on its own task); it works because `Delivery` carries the producing
//! span. The `correlation` field is recorded on `run`, both `actor.handle`s, and
//! `engine.route`, so a subscriber can group the whole run by run id.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  async_trait,
};
use fuchsia_actor_builtins::PassthroughCreator;
use fuchsia_engine::CorrelationId;
use fuchsia_engine::Engine;
use tokio::sync::Notify;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// What we record per span so a test can assert ancestry and field values.
#[derive(Clone, Debug, Default)]
struct SpanInfo {
  name: String,
  parent: Option<u64>,
  fields: HashMap<String, String>,
}

impl SpanInfo {
  fn field(&self, name: &str) -> Option<&str> {
    self.fields.get(name).map(String::as_str)
  }
}

type SpanMap = Arc<Mutex<HashMap<u64, SpanInfo>>>;

/// Records each span's name, parent id, and all field values (as strings).
#[derive(Clone, Default)]
struct Spans(SpanMap);

/// Captures every field of a span as a `name -> string` map, covering the value
/// kinds the runtime emits: display/debug (`%`/`?`), str, ints, bool.
#[derive(Default)]
struct FieldVisitor(HashMap<String, String>);
impl Visit for FieldVisitor {
  fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
    self.0.insert(field.name().to_owned(), format!("{value:?}"));
  }
  fn record_str(&mut self, field: &Field, value: &str) {
    self.0.insert(field.name().to_owned(), value.to_owned());
  }
  fn record_u64(&mut self, field: &Field, value: u64) {
    self.0.insert(field.name().to_owned(), value.to_string());
  }
  fn record_i64(&mut self, field: &Field, value: i64) {
    self.0.insert(field.name().to_owned(), value.to_string());
  }
  fn record_bool(&mut self, field: &Field, value: bool) {
    self.0.insert(field.name().to_owned(), value.to_string());
  }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Spans {
  fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
    // The explicit `parent:` if set (handle spans), else the contextual parent
    // (the entered root / enclosing span), else none.
    let parent = attrs
      .parent()
      .cloned()
      .or_else(|| ctx.current_span().id().cloned())
      .map(|p| p.into_u64());
    let mut visitor = FieldVisitor::default();
    attrs.record(&mut visitor);
    self.0.lock().unwrap().insert(
      id.into_u64(),
      SpanInfo {
        name: attrs.metadata().name().to_owned(),
        parent,
        fields: visitor.0,
      },
    );
  }

  // Fields recorded after creation (e.g. `engine.route`'s `correlation`,
  // `remove_graph`'s `nodes`) arrive here rather than in `on_new_span`.
  fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
    let mut visitor = FieldVisitor::default();
    values.record(&mut visitor);
    if let Some(info) = self.0.lock().unwrap().get_mut(&id.into_u64()) {
      info.fields.extend(visitor.0);
    }
  }
}

/// Collects each event's fields (including the `message` literal, which tracing
/// records under the `message` field) so a test can assert an event fired.
#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<HashMap<String, String>>>>);

impl<S: Subscriber> Layer<S> for Events {
  fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
    let mut visitor = FieldVisitor::default();
    event.record(&mut visitor);
    self.0.lock().unwrap().push(visitor.0);
  }
}

/// Terminal actor that signals when it has handled a message.
struct Sink(Arc<Notify>);
#[async_trait]
impl Actor for Sink {
  async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    self.0.notify_one();
    Ok(())
  }
}

struct SinkCreator(Arc<Notify>);
impl ActorCreator for SinkCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Sink(self.0.clone())))
  }
}

#[tokio::test]
async fn trace_and_correlation_follow_a_message_across_the_mailbox_hop() {
  let spans = Spans::default();
  // Current-thread runtime (tokio::test default) + thread-local subscriber, so
  // the spawned actor tasks share this subscriber. No level filter, so the TRACE
  // `engine.route` span is captured too.
  let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(spans.clone()));

  let notify = Arc::new(Notify::new());
  let engine = Engine::new();
  engine.register("passthrough", PassthroughCreator).await;
  engine.register("sink", SinkCreator(notify.clone())).await;

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
      "sink",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_default_edge(ActorId::new("a"), ActorId::new("b"))
    .unwrap();

  // Push within a root span; passthrough (a) re-emits → routes to sink (b).
  tracing::info_span!("ingress").in_scope(|| {
    engine
      .push(
        &ActorId::new("a"),
        Message::empty("ping"),
        CorrelationId::new(),
      )
      .unwrap();
  });

  tokio::time::timeout(Duration::from_secs(1), notify.notified())
    .await
    .expect("sink handled the message");

  // Assert the chain ingress → run → a.handle → engine.route → b.handle, and
  // that `correlation` is the same id at every level.
  let spans = spans.0.lock().unwrap();
  let by_name = |name: &str| -> Vec<(u64, SpanInfo)> {
    spans
      .iter()
      .filter(|(_, i)| i.name == name)
      .map(|(id, i)| (*id, i.clone()))
      .collect()
  };

  let ingress = spans
    .iter()
    .find(|(_, i)| i.name == "ingress" && i.parent.is_none())
    .map(|(id, _)| *id)
    .expect("ingress root span");

  let runs = by_name("run");
  assert_eq!(runs.len(), 1, "one run span");
  let (run_id, run_info) = &runs[0];
  assert_eq!(
    run_info.parent,
    Some(ingress),
    "run is parented by the ingress root"
  );
  let cid = run_info
    .field("correlation")
    .expect("run records the correlation field")
    .to_owned();

  let handles = by_name("actor.handle");
  assert_eq!(handles.len(), 2, "one handle span per actor");
  let (a_handle, a_info) = handles
    .iter()
    .find(|(_, i)| i.parent == Some(*run_id))
    .expect("upstream handle is parented by the run root");
  assert_eq!(
    a_info.field("correlation"),
    Some(cid.as_str()),
    "a.handle records the same correlation"
  );

  let routes = by_name("engine.route");
  let (route_id, route_info) = routes
    .iter()
    .find(|(_, i)| i.parent == Some(*a_handle))
    .expect("engine.route is parented by the upstream handle");
  assert_eq!(
    route_info.field("correlation"),
    Some(cid.as_str()),
    "engine.route records the same correlation"
  );

  let (_b_handle, b_info) = handles
    .iter()
    .find(|(_, i)| i.parent == Some(*route_id))
    .expect("downstream handle is parented by engine.route — trace crossed the hop");
  assert_eq!(
    b_info.field("correlation"),
    Some(cid.as_str()),
    "b.handle records the same correlation"
  );
}

#[tokio::test]
async fn control_plane_spans_carry_topology_fields() {
  let spans = Spans::default();
  let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(spans.clone()));

  let engine = Engine::new();
  engine.register("passthrough", PassthroughCreator).await;

  let a = ActorId::scoped("g", "a");
  let b = ActorId::scoped("g", "b");
  engine
    .add_node(
      a.clone(),
      "passthrough",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      b.clone(),
      "passthrough",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine.add_default_edge(a.clone(), b.clone()).unwrap();
  engine.remove_graph("g").await.unwrap();

  let spans = spans.0.lock().unwrap();
  let by_name =
    |name: &str| -> Vec<SpanInfo> { spans.values().filter(|i| i.name == name).cloned().collect() };

  // add_node: one span per node, recording the node id and the actor type.
  let add_nodes = by_name("add_node");
  assert_eq!(add_nodes.len(), 2, "one add_node span per node");
  assert!(
    add_nodes
      .iter()
      .all(|i| i.field("type") == Some("passthrough")),
    "add_node records the actor type as `type`"
  );
  assert!(
    add_nodes.iter().all(|i| i.fields.contains_key("node")),
    "add_node records the node id"
  );

  // add_edge: the default-port wiring records from/port/to, port == "out".
  let add_edges = by_name("add_edge");
  assert_eq!(add_edges.len(), 1, "one add_edge span");
  assert_eq!(
    add_edges[0].field("port"),
    Some("out"),
    "default edge wires the out port"
  );
  assert!(
    add_edges[0].fields.contains_key("from") && add_edges[0].fields.contains_key("to"),
    "add_edge records both endpoints"
  );

  // remove_graph: records the group and the count of nodes torn down.
  let removes = by_name("remove_graph");
  assert_eq!(removes.len(), 1, "one remove_graph span");
  assert_eq!(
    removes[0].field("group"),
    Some("g"),
    "remove_graph records the group"
  );
  assert_eq!(
    removes[0].field("nodes"),
    Some("2"),
    "remove_graph records the torn-down count"
  );
}

#[tokio::test]
async fn emit_no_route_event_fires_on_an_unwired_emit() {
  let events = Events::default();
  let _guard =
    tracing::subscriber::set_default(tracing_subscriber::registry().with(events.clone()));

  // A single passthrough node with no outgoing edge: it handles the pushed
  // message and re-emits on "out", which is wired nowhere → `emit.no_route`.
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
  engine
    .push(
      &ActorId::new("solo"),
      Message::empty("ping"),
      CorrelationId::new(),
    )
    .unwrap();

  // The emit happens on the actor's task; poll until the event shows up.
  for _ in 0..50 {
    let fired = events
      .0
      .lock()
      .unwrap()
      .iter()
      .any(|e| e.get("message").map(String::as_str) == Some("emit.no_route"));
    if fired {
      return;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
  }
  panic!(
    "emit.no_route never fired; events: {:?}",
    events.0.lock().unwrap()
  );
}
