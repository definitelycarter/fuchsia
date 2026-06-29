//! DAG enforcement: `add_edge` rejects any edge that would close a cycle
//! (self-loops, back-edges, longer cycles, and cycles that only show up when a
//! node's successors are flattened across its output ports), leaving the graph
//! unchanged — so a running graph is always acyclic.

use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Emit,
  Message, async_trait,
};
use fuchsia_engine::{Engine, EngineError};

// ---- A Dynamic source that re-emits each input on the port named by its type
//
// On `handle(msg)` it emits `msg` on the port `msg.type_`. Driving it with a
// message of type `"out"` makes it a relay on the `"out"` port, so a chain of
// these forwards a pushed message hop by hop — enough to observe routing
// outcomes through `route_counts`. Dynamic ports, so port validation never
// interferes with the cycle checks under test.

struct Relay {
  emit: Arc<dyn Emit>,
}

#[async_trait]
impl Actor for Relay {
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

struct RelayCreator;

impl ActorCreator for RelayCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Relay { emit: caps.emit() }))
  }
}

/// Build an engine with one `Relay` type and a node per id.
async fn engine_with_nodes(ids: &[&str]) -> Engine {
  let engine = Engine::new();
  engine.register("relay", RelayCreator).await;
  for id in ids {
    engine
      .add_node(
        ActorId::new(*id),
        "relay",
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
  }
  engine
}

// ---- Tests ------------------------------------------------------------------

#[tokio::test]
async fn self_loop_is_rejected() {
  let engine = engine_with_nodes(&["a"]).await;

  let err = engine
    .add_edge(ActorId::new("a"), "out", ActorId::new("a"))
    .unwrap_err();
  assert!(
    matches!(err, EngineError::Cycle { ref from, ref to } if from == &ActorId::new("a") && to == &ActorId::new("a")),
    "a self-loop is the trivial cycle and must be rejected: {err:?}"
  );
}

#[tokio::test]
async fn legal_dag_edges_are_accepted() {
  // A diamond: a -> b, a -> c, b -> d, c -> d. Every edge — including the
  // second one into `d` (a fan-in, not a cycle) — is a legal DAG edge.
  let engine = engine_with_nodes(&["a", "b", "c", "d"]).await;

  assert!(
    engine
      .add_edge(ActorId::new("a"), "out", ActorId::new("b"))
      .is_ok()
  );
  assert!(
    engine
      .add_edge(ActorId::new("a"), "out", ActorId::new("c"))
      .is_ok()
  );
  assert!(
    engine
      .add_edge(ActorId::new("b"), "out", ActorId::new("d"))
      .is_ok()
  );
  // A second edge into `d` from a different source — a diamond join, not a
  // cycle: `d` does not reach `c`, so this is accepted.
  assert!(
    engine
      .add_edge(ActorId::new("c"), "out", ActorId::new("d"))
      .is_ok()
  );
}

#[tokio::test]
async fn back_edge_closing_a_cycle_is_rejected_and_graph_unchanged() {
  // Chain a -> b -> c, then try the back-edge c -> a.
  let engine = engine_with_nodes(&["a", "b", "c"]).await;
  engine
    .add_edge(ActorId::new("a"), "out", ActorId::new("b"))
    .unwrap();
  engine
    .add_edge(ActorId::new("b"), "out", ActorId::new("c"))
    .unwrap();

  // c -> a closes the cycle (a already reaches c), so it is rejected.
  let err = engine
    .add_edge(ActorId::new("c"), "out", ActorId::new("a"))
    .unwrap_err();
  assert!(
    matches!(err, EngineError::Cycle { ref from, ref to } if from == &ActorId::new("c") && to == &ActorId::new("a")),
    "back-edge c -> a must be rejected with Cycle: {err:?}"
  );

  // The graph is unchanged: push into `a`, let it flow a -> b -> c, and check
  // that `c` still routes nowhere new (no edge c -> a was recorded). `c`'s emit
  // on "out" must be a no-route, not a delivery.
  engine
    .push(&ActorId::new("a"), Message::empty("out"))
    .unwrap();

  // Poll `c`'s "out" counter until its emission is recorded (bounded so a
  // regression fails fast rather than hanging).
  let mut c_out = engine.route_counts(&ActorId::new("c"), "out").unwrap();
  for _ in 0..100 {
    if c_out.delivered + c_out.shed + c_out.no_route >= 1 {
      break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    c_out = engine.route_counts(&ActorId::new("c"), "out").unwrap();
  }

  assert_eq!(
    c_out.no_route, 1,
    "c has no outgoing edge — its emit must be a no-route, proving c -> a was not added"
  );
  assert_eq!(
    c_out.delivered, 0,
    "the rejected back-edge must deliver nothing"
  );

  // The forward path is intact: a and b each delivered onward exactly once.
  assert_eq!(
    engine
      .route_counts(&ActorId::new("a"), "out")
      .unwrap()
      .delivered,
    1
  );
  assert_eq!(
    engine
      .route_counts(&ActorId::new("b"), "out")
      .unwrap()
      .delivered,
    1
  );
}

#[tokio::test]
async fn longer_cycle_a_b_c_a_is_caught() {
  // a -> b -> c built; the closing edge c -> a (a 3-node cycle) is rejected.
  let engine = engine_with_nodes(&["a", "b", "c"]).await;
  engine
    .add_edge(ActorId::new("a"), "out", ActorId::new("b"))
    .unwrap();
  engine
    .add_edge(ActorId::new("b"), "out", ActorId::new("c"))
    .unwrap();

  let err = engine
    .add_edge(ActorId::new("c"), "out", ActorId::new("a"))
    .unwrap_err();
  assert!(
    matches!(err, EngineError::Cycle { .. }),
    "a longer cycle a -> b -> c -> a must be caught: {err:?}"
  );
}

#[tokio::test]
async fn cross_port_cycle_is_caught() {
  // A cycle that only exists when a node's successors are flattened across all
  // of its ports. `a` fans out on two distinct ports (a real successor on
  // "side", the chain on "out"); `b` continues the chain on a *non-default*
  // port "alt". The path a -> b (on "out") -> c (on "alt") is only visible if
  // the reachability walk flattens every port — a walk that followed only the
  // "out" port would miss b's "alt" edge and wrongly allow c -> a.
  let engine = engine_with_nodes(&["a", "b", "c", "d"]).await;
  engine
    .add_edge(ActorId::new("a"), "side", ActorId::new("d"))
    .unwrap();
  engine
    .add_edge(ActorId::new("a"), "out", ActorId::new("b"))
    .unwrap();
  engine
    .add_edge(ActorId::new("b"), "alt", ActorId::new("c"))
    .unwrap();

  // c -> a closes the cross-port cycle a -(out)-> b -(alt)-> c -> a.
  let err = engine
    .add_edge(ActorId::new("c"), "out", ActorId::new("a"))
    .unwrap_err();
  assert!(
    matches!(err, EngineError::Cycle { ref from, ref to } if from == &ActorId::new("c") && to == &ActorId::new("a")),
    "a cycle reachable only by flattening multiple ports must be caught: {err:?}"
  );

  // The unrelated cross-port branch is unaffected: c -> d is a legal DAG edge
  // (d reaches nothing), so it is still accepted.
  assert!(
    engine
      .add_edge(ActorId::new("c"), "out", ActorId::new("d"))
      .is_ok()
  );
}
