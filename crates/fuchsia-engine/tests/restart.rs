//! Restart through the engine: `Engine::restart_node` revives a permanently-dead
//! node and force-restarts a live one, and a restart-enabled node rebuilds on a
//! transient crash with its mailbox surviving. The runtime owns the mechanism
//! (the supervisor); the engine owns the public face.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Backoff,
  Emit, FailurePolicy, Message, async_trait,
};
use fuchsia_actor_builtins::PassthroughCreator;
use fuchsia_engine::{CorrelationId, DeadLetter, DeadLettered, Engine, EngineError};
use tokio::sync::Notify;

/// Shared observation for a restart-supervised node under the engine.
struct Probe {
  /// `handle` calls 1..=`panic_first` panic; after that they record + succeed.
  panic_first: u32,
  handle_calls: AtomicU64,
  setups: AtomicU64,
  handled: Mutex<Vec<String>>,
  notify: Notify,
}

impl Probe {
  fn new(panic_first: u32) -> Arc<Self> {
    Arc::new(Self {
      panic_first,
      handle_calls: AtomicU64::new(0),
      setups: AtomicU64::new(0),
      handled: Mutex::new(Vec::new()),
      notify: Notify::new(),
    })
  }
}

struct ProbeActor {
  probe: Arc<Probe>,
}

#[async_trait]
impl Actor for ProbeActor {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    self.probe.setups.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let n = self.probe.handle_calls.fetch_add(1, Ordering::SeqCst) + 1;
    if (n as u32) <= self.probe.panic_first {
      panic!("intentional panic in handle (call {n})");
    }
    self.probe.handled.lock().unwrap().push(msg.type_.clone());
    self.probe.notify.notify_one();
    Ok(())
  }
}

struct ProbeCreator {
  probe: Arc<Probe>,
}

impl ActorCreator for ProbeCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(ProbeActor {
      probe: self.probe.clone(),
    }))
  }
}

/// A do-nothing dead-letter sink. Its only job in these tests is to be an
/// `Arc<dyn DeadLetter>` whose strong count we can watch: the restart
/// supervisor's recipe holds a clone, so the count dropping back to our own ref
/// proves the supervisor task was reaped (it dropped the recipe).
struct NoopSink;

impl DeadLetter for NoopSink {
  fn dead_letter(&self, _letter: DeadLettered) {}
}

fn restart_config(max_restarts: u32) -> ActorConfig {
  ActorConfig {
    failure: FailurePolicy::restart(max_restarts, Backoff::fixed(Duration::from_millis(1))),
    ..Default::default()
  }
}

/// Wait (polling, non-flaky) until `cond` holds, up to ~1s; returns whether it
/// did. Used to await async lifecycle transitions without a fixed sleep.
async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
  for _ in 0..400 {
    if cond() {
      return true;
    }
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
  cond()
}

#[tokio::test]
async fn restart_node_revives_a_permanently_dead_node() {
  // A node with a tiny budget that always panics first dies permanently after
  // exhausting it. `restart_node` (no force) revives it: it resolves again and,
  // now past its panic window, handles a message. Budget is reset.
  let engine = Engine::new();
  // panic_first = 2 → the first two incarnations crash (initial + 1 restart, the
  // budget), the node dies; a revive past the window then succeeds.
  let probe = Probe::new(2);
  engine
    .register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  engine
    .add_node(
      ActorId::new("p"),
      "probe",
      &restart_config(1), // 1 restart → 2 incarnations before permanent death
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Two crash messages exhaust the budget (one per incarnation).
  for label in ["crash-1", "crash-2"] {
    engine
      .push(
        &ActorId::new("p"),
        Message::empty(label),
        CorrelationId::new(),
      )
      .unwrap();
  }

  // Wait for the node to die (stops resolving as a router target).
  let died = wait_until(|| {
    engine
      .push(
        &ActorId::new("p"),
        Message::empty("probe"),
        CorrelationId::new(),
      )
      .is_err()
  })
  .await;
  assert!(died, "the node should die permanently after its budget");
  assert_eq!(probe.setups.load(Ordering::SeqCst), 2);

  // Revive it. No force needed for a dead node.
  engine
    .restart_node(&ActorId::new("p"), false)
    .await
    .unwrap();

  // It resolves again and, past its panic window, handles a message.
  let resolves = wait_until(|| {
    engine
      .push(
        &ActorId::new("p"),
        Message::empty("after-revive"),
        CorrelationId::new(),
      )
      .is_ok()
  })
  .await;
  assert!(resolves, "the revived node should resolve again");

  let handled = wait_until(|| !probe.handled.lock().unwrap().is_empty()).await;
  assert!(handled, "the revived node should handle a message");
  assert!(
    probe
      .handled
      .lock()
      .unwrap()
      .contains(&"after-revive".to_owned())
  );
  // The revive ran a third `setup` (the revived incarnation).
  assert_eq!(probe.setups.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn restart_node_force_restarts_a_live_node() {
  // A live, never-panicking node is force-restarted: it tears down + rebuilds
  // (a fresh `setup`), the mailbox surviving, and keeps handling.
  let engine = Engine::new();
  let probe = Probe::new(0); // never panics
  engine
    .register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  engine
    .add_node(
      ActorId::new("p"),
      "probe",
      &restart_config(3),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // It handles a first message normally.
  engine
    .push(
      &ActorId::new("p"),
      Message::empty("before"),
      CorrelationId::new(),
    )
    .unwrap();
  assert!(wait_until(|| probe.handled.lock().unwrap().len() == 1).await);
  assert_eq!(probe.setups.load(Ordering::SeqCst), 1);

  // Force-restart the live node.
  engine.restart_node(&ActorId::new("p"), true).await.unwrap();

  // A second `setup` ran (the rebuilt incarnation), and it still resolves +
  // handles — the mailbox/router entry survived.
  assert!(wait_until(|| probe.setups.load(Ordering::SeqCst) == 2).await);
  engine
    .push(
      &ActorId::new("p"),
      Message::empty("after"),
      CorrelationId::new(),
    )
    .unwrap();
  assert!(wait_until(|| probe.handled.lock().unwrap().contains(&"after".to_owned())).await);
}

#[tokio::test]
async fn restart_node_on_a_live_node_without_force_is_already_running() {
  // Without `force`, restarting a *live* node is rejected as already-running.
  let engine = Engine::new();
  let probe = Probe::new(0);
  engine
    .register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  engine
    .add_node(
      ActorId::new("p"),
      "probe",
      &restart_config(3),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  let err = engine
    .restart_node(&ActorId::new("p"), false)
    .await
    .unwrap_err();
  assert!(
    matches!(
      err,
      EngineError::Runtime(fuchsia_runtime::RuntimeError::AlreadyRunning(_))
    ),
    "a live node restarted without force is already-running, got {err:?}"
  );
}

#[tokio::test]
async fn restart_node_on_a_default_node_is_not_found() {
  // A default (restart-disabled) node has no restart handle, so `restart_node`
  // reports it as not found — it cannot be restarted.
  let engine = Engine::new();
  let probe = Probe::new(0);
  engine
    .register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  // Default config = max_restarts 0.
  engine
    .add_node(
      ActorId::new("p"),
      "probe",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  let err = engine
    .restart_node(&ActorId::new("p"), true)
    .await
    .unwrap_err();
  assert!(matches!(err, EngineError::NotFound(_)));
}

#[tokio::test]
async fn transient_crash_rebuilds_and_keeps_routing() {
  // A restart-enabled node that crashes once on its first message rebuilds and
  // drains the *next* queued message on the same mailbox — routing uninterrupted.
  let engine = Engine::new();
  let probe = Probe::new(1); // first handle panics, then recovers
  engine
    .register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  engine
    .add_node(
      ActorId::new("p"),
      "probe",
      &restart_config(3),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Queue the crasher + a survivor on the same mailbox.
  engine
    .push(
      &ActorId::new("p"),
      Message::empty("boom"),
      CorrelationId::new(),
    )
    .unwrap();
  engine
    .push(
      &ActorId::new("p"),
      Message::empty("survivor"),
      CorrelationId::new(),
    )
    .unwrap();

  // The fresh incarnation handles the survivor; the node never deregistered.
  assert!(
    wait_until(|| probe
      .handled
      .lock()
      .unwrap()
      .contains(&"survivor".to_owned()))
    .await
  );
  assert_eq!(probe.setups.load(Ordering::SeqCst), 2); // initial + 1 rebuild
  // Still resolves (a transient restart keeps the router entry).
  assert!(
    engine
      .push(
        &ActorId::new("p"),
        Message::empty("more"),
        CorrelationId::new()
      )
      .is_ok()
  );
}

#[tokio::test]
async fn removing_a_parked_dead_node_reaps_its_supervisor() {
  // A node that exhausts its budget parks (awaiting a revive), holding its rx +
  // rebuild recipe. Removing its graph instead of reviving must *reap* that
  // parked supervisor task, not leak it. We observe the reap through a
  // dead-letter sink `Arc` the recipe holds: while parked the supervisor keeps a
  // strong ref to it; once removal closes the surviving mailbox the supervisor
  // exits, drops the recipe, and the count falls back to our own ref. Without
  // the fix the parked task waits forever and the count stays elevated.
  let engine = Engine::new();
  let probe = Probe::new(u32::MAX); // always panics → exhausts any budget
  engine
    .register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;

  // A sink in the node's caps — the recipe pulls + holds it (plus the caps bag
  // it lives in), so the supervisor owns refs beyond our own.
  let sink: Arc<dyn DeadLetter> = Arc::new(NoopSink);
  let mut caps = ActorCapabilities::new();
  caps.insert::<dyn DeadLetter>(sink.clone());

  let id = ActorId::scoped("leak", "p");
  engine
    .add_node(id.clone(), "probe", &restart_config(1), caps) // 2 incarnations
    .await
    .unwrap();

  // One crash per incarnation exhausts the budget; the node then parks dead.
  for label in ["c1", "c2"] {
    engine
      .push(&id, Message::empty(label), CorrelationId::new())
      .unwrap();
  }
  assert!(
    wait_until(|| engine
      .push(&id, Message::empty("x"), CorrelationId::new())
      .is_err())
    .await,
    "the node should die permanently and stop resolving"
  );
  assert!(
    Arc::strong_count(&sink) > 1,
    "the parked supervisor's recipe should still hold the sink"
  );

  // Remove the graph rather than reviving. The engine drops its retained sender
  // (the last strong one), `rx` closes, and the parked supervisor exits.
  engine.remove_graph("leak").await.unwrap();
  assert!(
    wait_until(|| Arc::strong_count(&sink) == 1).await,
    "removing a parked-dead node must reap its supervisor (recipe released)"
  );
}

// ---- Graph integrity across restarts -----------------------------------------
//
// A restart must preserve the *whole* slice of the graph the node touches — both
// the edges *into* it (it still receives) and the edges *out* of it (it still
// emits). The bug these guard against: `deregister` used to drop a node's
// outgoing edges, and revival's `register` doesn't restore them, so a revived
// node could receive but its emits silently went nowhere (`no_route`).

/// Like `ProbeActor`, but also forwards each handled message to its `"out"` port
/// (via the engine-injected `emit`), so a downstream node confirms the node's
/// *outgoing* edge survived a restart/revival.
struct EmitProbeActor {
  probe: Arc<Probe>,
  emit: Arc<dyn Emit>,
}

#[async_trait]
impl Actor for EmitProbeActor {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    self.probe.setups.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let n = self.probe.handle_calls.fetch_add(1, Ordering::SeqCst) + 1;
    if (n as u32) <= self.probe.panic_first {
      panic!("intentional panic in handle (call {n})");
    }
    self.probe.handled.lock().unwrap().push(msg.type_.clone());
    self.probe.notify.notify_one();
    self.emit.emit(msg); // forward downstream on "out"
    Ok(())
  }
}

struct EmitProbeCreator {
  probe: Arc<Probe>,
}

impl ActorCreator for EmitProbeCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(EmitProbeActor {
      probe: self.probe.clone(),
      // The engine's `RoutedEmit` for this node, re-offered to each incarnation
      // from the same recipe caps — so a rebuilt/revived actor emits through the
      // same (surviving) edges.
      emit: caps.emit(),
    }))
  }
}

/// A terminal sink recording the type of every message it receives.
struct Sink {
  received: Arc<Mutex<Vec<String>>>,
  notify: Arc<Notify>,
}

#[async_trait]
impl Actor for Sink {
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.received.lock().unwrap().push(msg.type_.clone());
    self.notify.notify_one();
    Ok(())
  }
}

struct SinkCreator {
  received: Arc<Mutex<Vec<String>>>,
  notify: Arc<Notify>,
}

impl ActorCreator for SinkCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Sink {
      received: self.received.clone(),
      notify: self.notify.clone(),
    }))
  }
}

struct SinkHandles {
  received: Arc<Mutex<Vec<String>>>,
}

fn sink_node() -> (SinkCreator, SinkHandles) {
  let received = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());
  (
    SinkCreator {
      received: received.clone(),
      notify,
    },
    SinkHandles { received },
  )
}

#[tokio::test]
async fn revival_preserves_the_outgoing_edge() {
  // The focused regression: a revived node still emits to its successor. `p`
  // exhausts its budget and dies; after `restart_node` revives it, a message it
  // handles must reach the wired sink `s` — not silently `no_route`.
  let engine = Engine::new();
  let probe = Probe::new(2); // first 2 incarnations crash (budget), then succeed
  let (sink_creator, sink) = sink_node();
  engine
    .register(
      "emit-probe",
      EmitProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  engine.register("sink", sink_creator).await;

  engine
    .add_node(
      ActorId::new("p"),
      "emit-probe",
      &restart_config(1),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("s"),
      "sink",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_default_edge(ActorId::new("p"), ActorId::new("s"))
    .unwrap();

  // Exhaust the budget; the node dies and stops resolving.
  for label in ["crash-1", "crash-2"] {
    engine
      .push(
        &ActorId::new("p"),
        Message::empty(label),
        CorrelationId::new(),
      )
      .unwrap();
  }
  assert!(
    wait_until(|| engine
      .push(
        &ActorId::new("p"),
        Message::empty("x"),
        CorrelationId::new()
      )
      .is_err())
    .await,
    "the node should die permanently"
  );

  // Revive, then a handled message must flow out the surviving edge to the sink.
  engine
    .restart_node(&ActorId::new("p"), false)
    .await
    .unwrap();
  assert!(
    wait_until(|| engine
      .push(
        &ActorId::new("p"),
        Message::empty("after-revive"),
        CorrelationId::new()
      )
      .is_ok())
    .await
  );
  assert!(
    wait_until(|| sink
      .received
      .lock()
      .unwrap()
      .contains(&"after-revive".to_owned()))
    .await,
    "a revived node must still emit to its successor (outgoing edge preserved)"
  );
}

#[tokio::test]
async fn revival_preserves_the_full_path() {
  // The whole slice `up -> p -> s` must work after `p` dies and is revived: the
  // edge *into* p (up -> p) and the edge *out* of p (p -> s) both survive.
  let engine = Engine::new();
  let probe = Probe::new(2);
  let (sink_creator, sink) = sink_node();
  engine.register("up", PassthroughCreator).await;
  engine
    .register(
      "emit-probe",
      EmitProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  engine.register("sink", sink_creator).await;

  for (id, ty) in [("up", "up"), ("p", "emit-probe"), ("s", "sink")] {
    let cfg = if id == "p" {
      restart_config(1)
    } else {
      ActorConfig::default()
    };
    engine
      .add_node(ActorId::new(id), ty, &cfg, ActorCapabilities::new())
      .await
      .unwrap();
  }
  engine
    .add_default_edge(ActorId::new("up"), ActorId::new("p"))
    .unwrap();
  engine
    .add_default_edge(ActorId::new("p"), ActorId::new("s"))
    .unwrap();

  // Drive two crashes through the upstream to exhaust p's budget.
  for label in ["crash-1", "crash-2"] {
    engine
      .push(
        &ActorId::new("up"),
        Message::empty(label),
        CorrelationId::new(),
      )
      .unwrap();
  }
  assert!(
    wait_until(|| engine
      .push(
        &ActorId::new("p"),
        Message::empty("x"),
        CorrelationId::new()
      )
      .is_err())
    .await,
    "p should die after its budget"
  );

  engine
    .restart_node(&ActorId::new("p"), false)
    .await
    .unwrap();
  // Push through the upstream: it must reach the sink, exercising both p's
  // incoming and outgoing edges after the revival.
  assert!(
    wait_until(|| engine
      .push(
        &ActorId::new("up"),
        Message::empty("go"),
        CorrelationId::new()
      )
      .is_ok())
    .await
  );
  assert!(
    wait_until(|| sink.received.lock().unwrap().contains(&"go".to_owned())).await,
    "the full up -> p -> s path must be intact after p is revived"
  );
}

#[tokio::test]
async fn transient_restart_preserves_the_full_path() {
  // A transient crash-restart (no revival) must also keep the whole path intact:
  // up -> p -> s, where p crashes once and recovers, still delivers downstream.
  let engine = Engine::new();
  let probe = Probe::new(1); // first handle panics, then recovers
  let (sink_creator, sink) = sink_node();
  engine.register("up", PassthroughCreator).await;
  engine
    .register(
      "emit-probe",
      EmitProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  engine.register("sink", sink_creator).await;

  for (id, ty) in [("up", "up"), ("p", "emit-probe"), ("s", "sink")] {
    let cfg = if id == "p" {
      restart_config(3)
    } else {
      ActorConfig::default()
    };
    engine
      .add_node(ActorId::new(id), ty, &cfg, ActorCapabilities::new())
      .await
      .unwrap();
  }
  engine
    .add_default_edge(ActorId::new("up"), ActorId::new("p"))
    .unwrap();
  engine
    .add_default_edge(ActorId::new("p"), ActorId::new("s"))
    .unwrap();

  // The crasher is dropped on the transient restart; the survivor flows through.
  for label in ["boom", "survivor"] {
    engine
      .push(
        &ActorId::new("up"),
        Message::empty(label),
        CorrelationId::new(),
      )
      .unwrap();
  }
  assert!(
    wait_until(|| sink
      .received
      .lock()
      .unwrap()
      .contains(&"survivor".to_owned()))
    .await,
    "the full up -> p -> s path must survive a transient restart of p"
  );
}

// ---- Capability / lifecycle contract a product ingress cap relies on ----------
//
// Pins the restart lifecycle a host-inserted capability (e.g. an `IngressCap`
// that binds an external endpoint to this node at `setup`) depends on: restart
// re-runs `setup`, re-offers the *same* host capability handle, on the *same*
// node-id — and does **not** `teardown` a crashed incarnation, so the bind must
// be idempotent. A regression in any of these would silently break such a cap.

/// Stand-in for a product ingress capability: records the node-id bound at each
/// `setup` and counts `teardown`s (unbinds). Inserted into a node's caps under
/// its own type — exactly how a product inserts a domain capability.
struct BindRecorder {
  binds: Mutex<Vec<String>>,
  unbinds: AtomicU64,
}

impl BindRecorder {
  fn new() -> Arc<Self> {
    Arc::new(Self {
      binds: Mutex::new(Vec::new()),
      unbinds: AtomicU64::new(0),
    })
  }
}

/// Binds at `setup` (via the cap pulled from its caps bag), records the id, and
/// crashes on its first handle so we observe a rebuild.
struct BindActor {
  binder: Arc<BindRecorder>,
  probe: Arc<Probe>,
}

#[async_trait]
impl Actor for BindActor {
  async fn setup(&mut self, ctx: &ActorContext) -> Result<(), ActorError> {
    self
      .binder
      .binds
      .lock()
      .unwrap()
      .push(ctx.node_id.to_string());
    self.probe.setups.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let n = self.probe.handle_calls.fetch_add(1, Ordering::SeqCst) + 1;
    if (n as u32) <= self.probe.panic_first {
      panic!("intentional panic in handle (call {n})");
    }
    self.probe.handled.lock().unwrap().push(msg.type_.clone());
    self.probe.notify.notify_one();
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    self.binder.unbinds.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }
}

struct BindCreator {
  probe: Arc<Probe>,
}

impl ActorCreator for BindCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    // Pull the cap from the bag — so each rebuild getting the recorder proves the
    // *same* host capability is re-offered to every incarnation.
    let binder = caps
      .get::<BindRecorder>()
      .expect("the ingress cap is present in the node's caps");
    Ok(Box::new(BindActor {
      binder,
      probe: self.probe.clone(),
    }))
  }
}

#[tokio::test]
async fn restart_re_runs_setup_on_a_stable_id_with_the_same_caps_and_no_crash_teardown() {
  let engine = Engine::new();
  let probe = Probe::new(1); // first handle crashes (one transient restart), then recovers
  let binder = BindRecorder::new();
  engine
    .register(
      "bind",
      BindCreator {
        probe: probe.clone(),
      },
    )
    .await;

  // Insert the cap the way a product would — generic seam, its own type.
  let mut caps = ActorCapabilities::new();
  caps.insert::<BindRecorder>(binder.clone());
  engine
    .add_node(ActorId::new("p"), "bind", &restart_config(3), caps)
    .await
    .unwrap();

  // Crash once (→ rebuild), then a survivor that succeeds on the new incarnation.
  for label in ["boom", "survivor"] {
    engine
      .push(
        &ActorId::new("p"),
        Message::empty(label),
        CorrelationId::new(),
      )
      .unwrap();
  }
  assert!(
    wait_until(|| probe
      .handled
      .lock()
      .unwrap()
      .contains(&"survivor".to_owned()))
    .await
  );

  // `setup` re-ran on the rebuild — once per incarnation — and the same cap
  // recorded both, on the same stable node-id "p" (so a bind re-binds to the
  // same target).
  let binds = binder.binds.lock().unwrap();
  assert_eq!(binds.len(), 2, "setup must re-run on each incarnation");
  assert!(
    binds.iter().all(|id| id == "p"),
    "the node-id is stable across incarnations, got {binds:?}"
  );
  // The crashed incarnation was NOT torn down — so the re-`setup` has no matching
  // unbind, which is exactly why a real ingress cap must be idempotent.
  assert_eq!(
    binder.unbinds.load(Ordering::SeqCst),
    0,
    "a crash restart must not run teardown on the poisoned incarnation"
  );
}

#[tokio::test]
async fn re_delivery_crash_with_poison_disabled_still_dies_not_loops() {
  // Mechanism B (sparing a re-delivery crash from the restart budget) must apply
  // ONLY when poison quarantine is enabled. With `poison_after == 0` (the
  // default) there is no mechanism-A gate to ever divert a re-delivered poison
  // message, so a re-delivery crash must still charge the budget — the node dies
  // after `max_restarts` rather than rebuilding forever. (Regression guard: the
  // un-gated rule would free-rebuild a re-delivered crash indefinitely.)
  let engine = Engine::new();
  let probe = Probe::new(u32::MAX); // always panics
  engine
    .register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    )
    .await;
  // Restart enabled (max 2), poison quarantine left disabled (default).
  engine
    .add_node(
      ActorId::new("p"),
      "probe",
      &restart_config(2),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Feed re-deliveries (attempts > 1) of a message that always crashes. Each
  // crash charges the budget (poison disabled), so within `max_restarts + 1`
  // crashes the node is permanently dead. Extra pushes after death resolve to
  // NotFound — harmless.
  for _ in 0..5 {
    let _ = engine
      .push_durable_attempt(
        &ActorId::new("p"),
        Message::empty("poison"),
        CorrelationId::new(),
        2, // a re-delivery
      )
      .await;
  }

  assert!(
    wait_until(|| engine
      .push(
        &ActorId::new("p"),
        Message::empty("x"),
        CorrelationId::new()
      )
      .is_err())
    .await,
    "a re-delivery crash with poison disabled must still exhaust the budget and die"
  );
}

/// A node whose `teardown` panics but whose `handle` is fine.
struct TeardownPanicActor {
  probe: Arc<Probe>,
}

#[async_trait]
impl Actor for TeardownPanicActor {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    self.probe.setups.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.probe.handled.lock().unwrap().push(msg.type_.clone());
    self.probe.notify.notify_one();
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    panic!("intentional panic in teardown");
  }
}

struct TeardownPanicCreator {
  probe: Arc<Probe>,
}

impl ActorCreator for TeardownPanicCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(TeardownPanicActor {
      probe: self.probe.clone(),
    }))
  }
}

#[tokio::test]
async fn teardown_panic_on_a_restart_node_does_not_zombify_it() {
  // A panicking `teardown` must not unwind the restart supervisor (which has no
  // JoinHandle watcher behind it) and silently zombify the node. A force-restart
  // runs `teardown`; with the panic caught, the node still rebuilds and serves.
  let engine = Engine::new();
  let probe = Probe::new(0); // `handle` never panics — only `teardown` does
  engine
    .register(
      "td-panic",
      TeardownPanicCreator {
        probe: probe.clone(),
      },
    )
    .await;
  engine
    .add_node(
      ActorId::new("p"),
      "td-panic",
      &restart_config(3),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Handle a first message (setup #1).
  engine
    .push(
      &ActorId::new("p"),
      Message::empty("before"),
      CorrelationId::new(),
    )
    .unwrap();
  assert!(wait_until(|| probe.handled.lock().unwrap().contains(&"before".to_owned())).await);

  // Force-restart → `teardown` panics (must be caught) → the node rebuilds.
  engine.restart_node(&ActorId::new("p"), true).await.unwrap();
  assert!(
    wait_until(|| probe.setups.load(Ordering::SeqCst) == 2).await,
    "the node must rebuild despite a panicking teardown — not be silently zombified"
  );

  // And it still resolves + handles after the rebuild.
  engine
    .push(
      &ActorId::new("p"),
      Message::empty("after"),
      CorrelationId::new(),
    )
    .unwrap();
  assert!(
    wait_until(|| probe.handled.lock().unwrap().contains(&"after".to_owned())).await,
    "the rebuilt node must keep serving after a caught teardown panic"
  );
}
