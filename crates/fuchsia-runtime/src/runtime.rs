use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorFactory, ActorId,
  Message, OutputPorts,
};
use fuchsia_transport::{Ack, Delivery, Health, MailboxRx, MailboxTx, mailbox};

use crate::error::RuntimeError;
use crate::registry::{ActorHandle, ActorRegistry};
use crate::schedule::TokioSchedule;

/// A reaction to a node's death, installed on the [`Runtime`] by the layer above
/// it (the engine). Called **once**, on the supervisor task, with the dead
/// node's [`ActorId`] when its actor task exits abnormally (a panic, or its
/// senders vanishing without an explicit stop) — *not* on a clean
/// stop/teardown.
///
/// This is the cross-layer seam death detection needs: the runtime owns the
/// task and detects the exit, but only the engine owns the [`RouterState`] that
/// `push`/`route` resolve against, so it must be told to drop the node so it
/// stops resolving as a routable target. A callback (rather than a watch
/// channel) is used so the engine *reacts* without spawning a poller, and so the
/// restart supervisor (a later slice) can extend the same per-node seam.
///
/// [`RouterState`]: ../../fuchsia_engine/index.html
pub type DeathListener = Arc<dyn Fn(&ActorId) + Send + Sync>;

pub struct Runtime {
  factory: ActorFactory,
  // Shared with each node's supervisor task so that, on a detected death, the
  // supervisor can deregister the node from the runtime's own address book —
  // keeping it consistent with the engine's router and giving `deliver`/`stop`
  // an honest view of liveness. An `Arc<Mutex<…>>` because the supervisors are
  // detached tasks that outlive any single `&mut self` borrow.
  registry: Arc<Mutex<ActorRegistry>>,
  // Installed by the engine via `on_death`; invoked by a supervisor when its
  // node dies, so the engine can drop the node from its router.
  death_listener: Option<DeathListener>,
}

impl Runtime {
  pub fn new() -> Self {
    Self {
      factory: ActorFactory::new(),
      registry: Arc::new(Mutex::new(ActorRegistry::new())),
      death_listener: None,
    }
  }

  pub fn register(&mut self, type_name: impl Into<String>, creator: impl ActorCreator) {
    self.factory.register(type_name, creator);
  }

  /// Install the reaction run when a node dies. The engine installs a listener
  /// that deregisters the dead node from its router so it stops resolving as a
  /// routable target. See [`DeathListener`].
  pub fn on_death(&mut self, listener: DeathListener) {
    self.death_listener = Some(listener);
  }

  pub async fn spawn(
    &mut self,
    actor_id: ActorId,
    type_name: &str,
    config: &ActorConfig,
  ) -> Result<(), RuntimeError> {
    self
      .spawn_with_caps(actor_id, type_name, config, ActorCapabilities::new())
      .await
      .map(|_| ())
  }

  fn context(actor_id: &ActorId) -> ActorContext {
    // Built once per actor at spawn time. The `node_id` is the actor's stable
    // identity that every per-message context shares (an `Arc::clone` — a
    // refcount bump, not a re-allocation); we allocate the `Arc<str>` once here.
    let id: Arc<str> = Arc::from(actor_id.to_string());
    // Refcount bump of the one allocation — the spawn-time context is built once
    // per actor, so the cost is negligible; the two string ids share storage.
    // `task_id` is a placeholder `0` here: this base context is never handed to a
    // `handle` (each delivery rebuilds it with a fresh counter); it only seeds
    // `setup`/`teardown`, where "this handling" has no meaning.
    ActorContext::new(id.clone(), id, 0)
  }

  /// Build an actor and its mailbox **without** running `setup` or registering
  /// it — the first half of a spawn. The mailbox/health exist before
  /// construction so the scheduler can hold a weak handle to this actor's own
  /// mailbox (timers deliver there), then the actor is built with the full
  /// capability bundle. Pair with [`Spawning::setup`] (run *outside* any runtime
  /// lock) and [`Runtime::commit`].
  pub fn prepare(
    &mut self,
    actor_id: ActorId,
    type_name: &str,
    config: &ActorConfig,
    caps: ActorCapabilities,
  ) -> Result<Spawning, RuntimeError> {
    if self.registry_contains(&actor_id)? {
      return Err(RuntimeError::AlreadyRunning(actor_id));
    }

    let (tx, rx) = mailbox(32);
    let health = Arc::new(Health::default());
    let caps = caps.with_schedule(Arc::new(TokioSchedule {
      mailbox: tx.downgrade(),
      health: health.clone(),
    }));

    let actor = self.factory.create(type_name, config, &caps)?;
    // Resolve the node's declared output ports from the same creator, so the
    // engine can validate edges against them. Same name-keyed lookup `create`
    // just did, so the two cannot drift.
    let output_ports = self.factory.output_ports(type_name, config)?;
    let ctx = Self::context(&actor_id);

    Ok(Spawning {
      actor,
      ctx,
      actor_id,
      type_name: type_name.to_owned(),
      tx,
      rx,
      health,
      output_ports,
    })
  }

  /// Spawn a prepared (and set-up) actor's receive loop and register it as a
  /// routable target — the second half of a spawn. Re-checks for a duplicate id:
  /// another spawn may have committed the same id while `setup` ran outside the
  /// lock, in which case the prepared actor is dropped.
  ///
  /// Hands back the node's declared [`OutputPorts`] alongside its mailbox/health
  /// so the engine can store the declaration and validate edges against it.
  pub fn commit(
    &mut self,
    spawning: Spawning,
  ) -> Result<(MailboxTx, Arc<Health>, OutputPorts), RuntimeError> {
    let Spawning {
      actor,
      ctx,
      actor_id,
      type_name,
      tx,
      rx,
      health,
      output_ports,
    } = spawning;

    let mut registry = self.registry.lock().map_err(|_| RuntimeError::Lock)?;
    if registry.contains(&actor_id) {
      return Err(RuntimeError::AlreadyRunning(actor_id));
    }

    let handle = ActorHandle::new(
      // `ActorId` is `String`-backed, so this is a real clone, not a refcount
      // bump — but `commit` is a cold per-spawn path and the supervisor needs an
      // owned id of its own (it outlives this call), matching how `add_node` /
      // `register` already clone the id on these cold paths.
      actor_id.clone(),
      type_name,
      // Refcount bump of the mpsc sender so the registry keeps a routable copy
      // while the caller (the engine) gets one for its router.
      tx.clone(),
      // Refcount bump of the shared health counters.
      health.clone(),
    );

    // Keep the actor's `JoinHandle` (previously discarded) and hand it, with the
    // node's identity / health / stop flag, to a per-node **supervisor** task.
    // The supervisor awaits the actor task and turns its exit into a lifecycle
    // event: a panic (or an abnormal exit) deregisters the node and records a
    // death, while a clean stop does neither. This is also the seam the future
    // restart slice hooks into — the supervisor is the natural owner of the
    // handle and the rebuild recipe.
    let actor_task = tokio::spawn(run_actor(actor, ctx, rx));
    tokio::spawn(supervise(
      actor_task,
      actor_id,
      // Refcount bumps: the supervisor shares the same health counters and stop
      // flag as the registry handle.
      health.clone(),
      handle.stopping(),
      // A **weak** handle to the registry, not a strong one: the registry holds
      // every node's mailbox sender, so a strong ref here would keep all those
      // senders alive and a dropped `Runtime` could never close its actors'
      // mailboxes (teardown would never run). Weak lets the registry drop with
      // the `Runtime`; on death the supervisor upgrades to deregister, and a
      // gone registry just means the whole runtime is already torn down.
      Arc::downgrade(&self.registry),
      // Refcount bump of the installed listener, if any.
      self.death_listener.clone(),
    ));

    registry.insert(handle);
    drop(registry);

    Ok((tx, health, output_ports))
  }

  /// Spawn an actor end to end — [`prepare`](Self::prepare), `setup`,
  /// [`commit`](Self::commit) — for direct callers that hold no external lock
  /// (tests, a standalone runtime). The engine instead drives the three steps
  /// itself so `setup` runs *outside* its runtime lock and a slow async setup
  /// can't serialize other graph mutations.
  pub async fn spawn_with_caps(
    &mut self,
    actor_id: ActorId,
    type_name: &str,
    config: &ActorConfig,
    caps: ActorCapabilities,
  ) -> Result<(MailboxTx, Arc<Health>, OutputPorts), RuntimeError> {
    let mut spawning = self.prepare(actor_id, type_name, config, caps)?;
    spawning.setup().await?;
    self.commit(spawning)
  }

  pub async fn deliver(&self, actor_id: &ActorId, msg: Message) -> Result<(), RuntimeError> {
    // Resolve the mailbox + health under the registry lock, then release it
    // *before* the `.await`: holding a `std::sync::Mutex` guard across an await
    // would make this future `!Send`. The mailbox/health are cheap refcount
    // bumps. A node a supervisor has already deregistered (a death) is gone from
    // the registry, so this is `ActorNotFound` — it no longer resolves.
    let (mailbox, health) = {
      let registry = self.registry.lock().map_err(|_| RuntimeError::Lock)?;
      let handle = registry
        .get(actor_id)
        .ok_or_else(|| RuntimeError::ActorNotFound(actor_id.clone()))?;
      // Refcount bumps of the mpsc sender and the shared health counters.
      (handle.mailbox().clone(), handle.health().clone())
    };

    let delivery = Delivery::new(msg, Ack::Health(health));
    mailbox
      .send(delivery)
      .await
      .map_err(|_| RuntimeError::Send("mailbox closed".to_owned()))
  }

  pub fn stop(&mut self, actor_id: &ActorId) -> Result<(), RuntimeError> {
    let mut registry = self.registry.lock().map_err(|_| RuntimeError::Lock)?;
    let handle = registry
      .remove(actor_id)
      .ok_or_else(|| RuntimeError::ActorNotFound(actor_id.clone()))?;
    // Mark the node as intentionally stopping *before* its mailbox sender drops,
    // so when the run loop then exits on its closed `rx` the supervisor reads a
    // clean stop and does not count it as a death. Dropping the handle closes
    // tx, which closes rx in the task, causing the actor loop to exit and
    // teardown to run.
    handle.mark_stopping();
    Ok(())
  }

  /// Whether the registry currently holds `id`. A small helper so the lock
  /// (and its poison handling) lives in one place.
  fn registry_contains(&self, id: &ActorId) -> Result<bool, RuntimeError> {
    Ok(
      self
        .registry
        .lock()
        .map_err(|_| RuntimeError::Lock)?
        .contains(id),
    )
  }
}

impl Default for Runtime {
  fn default() -> Self {
    Self::new()
  }
}

/// An actor created but not yet running, produced by [`Runtime::prepare`]: the
/// [`Actor`] instance, its identity, and its mailbox. The caller runs
/// [`setup`](Spawning::setup) on it **without holding any runtime lock**, then
/// re-locks to [`Runtime::commit`] it. This is what keeps a slow async `setup`
/// (one that does I/O) from serializing every other graph mutation behind the
/// runtime lock.
pub struct Spawning {
  actor: Box<dyn Actor>,
  ctx: ActorContext,
  actor_id: ActorId,
  type_name: String,
  tx: MailboxTx,
  rx: MailboxRx,
  health: Arc<Health>,
  output_ports: OutputPorts,
}

impl Spawning {
  /// Run the actor's `setup`. Call this *outside* any runtime lock; on failure
  /// the `Spawning` is dropped (its actor's `Drop` releases partial state) and
  /// nothing is registered.
  pub async fn setup(&mut self) -> Result<(), RuntimeError> {
    self
      .actor
      .setup(&self.ctx)
      .await
      .map_err(RuntimeError::Actor)
  }
}

async fn run_actor(mut actor: Box<dyn Actor>, ctx: ActorContext, mut rx: MailboxRx) {
  use tracing::Instrument;
  while let Some(delivery) = rx.recv().await {
    let Delivery {
      msg,
      ack,
      span: parent,
      correlation,
    } = delivery;
    // The handle span is a child of the upstream's span (carried on the
    // delivery), so a trace follows the message across this mailbox hop. The
    // actor's own emits, made inside this span, propagate it onward. DEBUG so
    // it's off the hot path unless tracing is turned up.
    let span =
      tracing::debug_span!(parent: &parent, "actor.handle", node = %ctx.node_id, kind = %msg.type_);

    // Build a **per-delivery** context, finally giving the three id fields
    // distinct meanings: `node_id` static (which actor — the stable spawn-time
    // id), `execution_id` the run this message belongs to (the delivery's
    // correlation), `task_id` this handling (a fresh per-message id).
    //
    // Both shared ids are `Arc<str>` refcount bumps, not allocations:
    // `execution_id` is the correlation's inner arc (taken *now*, before
    // `correlation.scope(...)` below moves the correlation), and `node_id` is an
    // `Arc::clone` of the actor's stable id. `task_id` is a bare `u64` counter —
    // no allocation either; the `"task-N"` string is rendered lazily, only if a
    // guest host reads it (`ActorContext::task_label`). So the per-message
    // context build now allocates nothing.
    let execution_id = correlation.as_arc(); // refcount bump, before the move below
    let node_id = Arc::clone(&ctx.node_id); // refcount bump of the stable spawn-time id
    let msg_ctx = ActorContext::new(execution_id, node_id, next_task_id());

    // Enter the correlation for the handle — a task-local mirroring the span, so
    // emits the actor makes inside `handle` capture this run id and propagate it
    // onward. `.instrument(span).await` enters the span for the duration of the
    // async handle without holding a `!Send` span guard across the await point.
    let outcome = correlation
      .scope(actor.handle(&msg_ctx, msg).instrument(span))
      .await;
    ack.report(outcome);
  }

  let _ = actor.teardown(&ctx).await;
}

/// Watch one actor's task and turn its exit into a node-lifecycle event.
///
/// Holds the actor task's `JoinHandle` (no longer discarded at spawn) and awaits
/// it. The task exits one of two ways:
///
/// - **Panic** — `handle`/`setup`/`teardown` unwound, so `join` resolves to
///   `Err(JoinError)`. Always a death: `teardown` never ran and the node is
///   permanently dead.
/// - **Loop exit** — `rx` closed (every sender dropped) so the run loop ended
///   and `teardown` ran; `join` resolves to `Ok(())`. This is a *death* only if
///   the node was not intentionally stopped — i.e. the `stopping` flag is unset,
///   meaning its senders vanished without a `Runtime::stop` (e.g. the registry
///   entry was dropped out from under it). An intentional stop set the flag, so
///   that case is a clean shutdown and is **not** counted as a death.
///
/// On a death the supervisor: records it on the node's [`Health`] (the distinct
/// `died` counter, not `errored`), deregisters the node from the runtime's
/// [`ActorRegistry`] so it stops resolving for `deliver`, and fires the
/// [`DeathListener`] so the engine drops it from its router (so routed
/// deliveries stop resolving to a dead mailbox). The deregistration is the seam
/// the future restart slice extends — instead of only dropping the node, the
/// supervisor will rebuild it on the surviving mailbox.
async fn supervise(
  actor_task: tokio::task::JoinHandle<()>,
  actor_id: ActorId,
  health: Arc<Health>,
  stopping: Arc<AtomicBool>,
  registry: Weak<Mutex<ActorRegistry>>,
  death_listener: Option<DeathListener>,
) {
  let join = actor_task.await;

  // Upgrade the weak registry handle. If it fails the whole `Runtime` has been
  // dropped — a *global* teardown, not this one node dying — so its actors'
  // mailboxes closed on purpose and a clean (`Ok`) exit here is not a death.
  let registry = registry.upgrade();

  // Classify the exit:
  // - A panic (`Err`) is always a death — `teardown` never ran and the node is
  //   permanently dead, even mid-teardown.
  // - A clean loop exit (`Ok`) is a death only when the node was *not*
  //   intentionally stopped (`stopping` unset) *and* the runtime is still up
  //   (the registry upgraded) — i.e. its senders vanished out from under a live
  //   runtime. An intentional stop, or a runtime-wide drop, is a clean shutdown.
  let died = match &join {
    Err(_panic) => true,
    Ok(()) => !stopping.load(Ordering::SeqCst) && registry.is_some(),
  };
  if !died {
    return;
  }

  if let Err(join_err) = &join {
    // The panic was swallowed before (the `JoinHandle` was discarded); surface
    // it so a dead node is not silent.
    tracing::error!(node = %actor_id, error = %join_err, "actor task died (panic)");
  } else {
    tracing::error!(node = %actor_id, "actor task exited unexpectedly");
  }

  // Observable as a distinct death on the node's shared `Health` (the `died`
  // counter, not `errored`).
  health.record_death();

  // Deregister from the runtime's address book so the node stops resolving for
  // `deliver`. Best-effort on a poisoned lock — the death is already recorded on
  // `Health`, and a poisoned registry means the process is already unwinding.
  if let Some(registry) = &registry {
    if let Ok(mut registry) = registry.lock() {
      registry.remove(&actor_id);
    }
  }

  // Tell the layer above (the engine) so it drops the node from its router,
  // where routed deliveries actually resolve. Runs last so the runtime's own
  // state is consistent first.
  if let Some(listener) = death_listener {
    listener(&actor_id);
  }
}

/// A fresh, process-unique task id for one `handle` invocation. Monotonic, so
/// each message's `task_id` is distinct. A bare `u64` — no allocation; the
/// guest-visible `"task-N"` string is rendered lazily (`ActorContext::task_label`)
/// only when a host actually reads it.
fn next_task_id() -> u64 {
  static NEXT: AtomicU64 = AtomicU64::new(1);
  NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
  use super::*;
  use fuchsia_actor::{ActorError, ActorId, MessageValue, Schedule, async_trait};
  use std::sync::Arc;
  use std::sync::Mutex;
  use std::sync::atomic::{AtomicBool, Ordering};
  use tokio::sync::Notify;

  // ---- Echo actor (used by the basic tests) ----

  struct EchoActor;

  #[async_trait]
  impl Actor for EchoActor {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
    async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
      Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
  }

  struct EchoCreator;

  impl ActorCreator for EchoCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(EchoActor))
    }
  }

  // ---- Probe actor (observes lifecycle events) ----

  struct Probe {
    setup_called: AtomicBool,
    teardown_called: AtomicBool,
    received: Mutex<Vec<Message>>,
    notify: Notify,
  }

  impl Probe {
    fn new() -> Arc<Self> {
      Arc::new(Self {
        setup_called: AtomicBool::new(false),
        teardown_called: AtomicBool::new(false),
        received: Mutex::new(Vec::new()),
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
      self.probe.setup_called.store(true, Ordering::SeqCst);
      Ok(())
    }
    async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
      self.probe.received.lock().unwrap().push(msg);
      self.probe.notify.notify_one();
      Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      self.probe.teardown_called.store(true, Ordering::SeqCst);
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

  // ---- Failing-setup actor (for the setup-failure scenario) ----

  struct FailingSetupActor;

  #[async_trait]
  impl Actor for FailingSetupActor {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Err(ActorError::Setup("intentional".to_owned()))
    }
    async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
      Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
  }

  struct FailingSetupCreator;

  impl ActorCreator for FailingSetupCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(FailingSetupActor))
    }
  }

  // ---- Helpers ----

  fn runtime() -> Runtime {
    let mut rt = Runtime::new();
    rt.register("echo", EchoCreator);
    rt
  }

  fn actor_id(s: &str) -> ActorId {
    ActorId::new(s)
  }

  // ---- Basic tests ----

  #[tokio::test]
  async fn spawn_registers_actor() {
    let mut rt = runtime();
    rt.spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .unwrap();
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn spawn_duplicate_returns_error() {
    let mut rt = runtime();
    rt.spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .unwrap();
    let err = rt
      .spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::AlreadyRunning(_)));
  }

  #[tokio::test]
  async fn spawn_unknown_type_returns_error() {
    let mut rt = runtime();
    let err = rt
      .spawn(actor_id("a"), "unknown", &ActorConfig::default())
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::Actor(_)));
  }

  #[tokio::test]
  async fn deliver_to_running_actor_succeeds() {
    let mut rt = runtime();
    rt.spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .unwrap();
    let result = rt.deliver(&actor_id("a"), Message::empty("test")).await;
    assert!(result.is_ok());
  }

  #[tokio::test]
  async fn deliver_to_missing_actor_returns_error() {
    let rt = runtime();
    let err = rt
      .deliver(&actor_id("missing"), Message::empty("test"))
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::ActorNotFound(_)));
  }

  #[tokio::test]
  async fn stop_unregisters_actor() {
    let mut rt = runtime();
    rt.spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .unwrap();
    rt.stop(&actor_id("a")).unwrap();
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn stop_missing_actor_returns_error() {
    let mut rt = runtime();
    let err = rt.stop(&actor_id("missing")).err().unwrap();
    assert!(matches!(err, RuntimeError::ActorNotFound(_)));
  }

  // ---- Lifecycle tests ----

  #[tokio::test]
  async fn spawn_calls_setup() {
    let probe = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    );
    rt.spawn(actor_id("a"), "probe", &ActorConfig::default())
      .await
      .unwrap();
    // setup runs synchronously inside spawn, so this is observable immediately
    assert!(probe.setup_called.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn handle_receives_message() {
    let probe = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    );
    rt.spawn(actor_id("a"), "probe", &ActorConfig::default())
      .await
      .unwrap();

    let msg = Message::json("test", serde_json::json!({"value": 42}));
    rt.deliver(&actor_id("a"), msg).await.unwrap();

    probe.notify.notified().await;

    let received = probe.received.lock().unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].type_, "test");
    assert!(matches!(received[0].value, MessageValue::Json(_)));
  }

  #[tokio::test]
  async fn stop_triggers_teardown() {
    let probe = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    );
    rt.spawn(actor_id("a"), "probe", &ActorConfig::default())
      .await
      .unwrap();
    rt.stop(&actor_id("a")).unwrap();

    probe.notify.notified().await;

    assert!(probe.teardown_called.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn dropping_runtime_triggers_teardown() {
    let probe = Probe::new();
    {
      let mut rt = Runtime::new();
      rt.register(
        "probe",
        ProbeCreator {
          probe: probe.clone(),
        },
      );
      rt.spawn(actor_id("a"), "probe", &ActorConfig::default())
        .await
        .unwrap();
    }
    // rt is dropped here; the handle's tx is dropped; the actor task
    // sees rx close and runs teardown

    probe.notify.notified().await;

    assert!(probe.teardown_called.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn multiple_actors_run_independently() {
    let probe_a = Probe::new();
    let probe_b = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe_a",
      ProbeCreator {
        probe: probe_a.clone(),
      },
    );
    rt.register(
      "probe_b",
      ProbeCreator {
        probe: probe_b.clone(),
      },
    );

    rt.spawn(actor_id("a"), "probe_a", &ActorConfig::default())
      .await
      .unwrap();
    rt.spawn(actor_id("b"), "probe_b", &ActorConfig::default())
      .await
      .unwrap();

    rt.deliver(&actor_id("a"), Message::empty("for-a"))
      .await
      .unwrap();
    probe_a.notify.notified().await;

    assert_eq!(probe_a.received.lock().unwrap().len(), 1);
    assert_eq!(probe_a.received.lock().unwrap()[0].type_, "for-a");
    assert!(probe_b.received.lock().unwrap().is_empty());
  }

  #[tokio::test]
  async fn setup_failure_does_not_register() {
    let mut rt = Runtime::new();
    rt.register("failing", FailingSetupCreator);

    let err = rt
      .spawn(actor_id("a"), "failing", &ActorConfig::default())
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::Actor(ActorError::Setup(_))));
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());
  }

  // ---- Death detection ----

  /// An actor whose `handle` panics on the first message — the zombie-maker the
  /// RFC's death detection closes.
  struct PanicActor;

  #[async_trait]
  impl Actor for PanicActor {
    async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
      panic!("intentional panic in handle")
    }
  }

  struct PanicCreator;

  impl ActorCreator for PanicCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(PanicActor))
    }
  }

  /// Install a death listener that records the dead ids and notifies, so a test
  /// can await the death signal rather than sleep.
  fn record_deaths(rt: &mut Runtime) -> (Arc<Mutex<Vec<ActorId>>>, Arc<Notify>) {
    let dead = Arc::new(Mutex::new(Vec::new()));
    let notify = Arc::new(Notify::new());
    let dead_for_cb = dead.clone();
    let notify_for_cb = notify.clone();
    rt.on_death(Arc::new(move |id: &ActorId| {
      dead_for_cb.lock().unwrap().push(id.clone());
      notify_for_cb.notify_one();
    }));
    (dead, notify)
  }

  #[tokio::test]
  async fn panicking_handle_is_detected_as_a_death() {
    let mut rt = Runtime::new();
    rt.register("panic", PanicCreator);
    let (dead, notify) = record_deaths(&mut rt);

    let (tx, health, _ports) = rt
      .spawn_with_caps(
        actor_id("a"),
        "panic",
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
    // The registry holds its own sender; drop the caller's so the mailbox isn't
    // kept open by this test (matching how a real spawner hands the tx onward).
    drop(tx);

    // Deliver a message; handling it panics, unwinding the task.
    rt.deliver(&actor_id("a"), Message::empty("boom"))
      .await
      .unwrap();

    // The death signal fires: the supervisor saw the task die.
    notify.notified().await;

    // Observable on Health as a distinct death (not an errored message).
    assert_eq!(health.died(), 1);
    assert_eq!(health.errored(), 0);
    // The listener was told which node died.
    assert_eq!(dead.lock().unwrap().as_slice(), &[actor_id("a")]);
    // The node stops resolving as a routable target in the runtime: deliver now
    // reports it as gone.
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());
    let err = rt
      .deliver(&actor_id("a"), Message::empty("again"))
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::ActorNotFound(_)));
  }

  #[tokio::test]
  async fn normal_stop_runs_teardown_and_is_not_a_death() {
    let probe = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    );
    let (dead, _notify) = record_deaths(&mut rt);

    let (tx, health, _ports) = rt
      .spawn_with_caps(
        actor_id("a"),
        "probe",
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .unwrap();
    // Drop the caller's sender so `stop` (which drops the registry's sender) is
    // enough to close the mailbox and let the run loop reach teardown.
    drop(tx);

    rt.stop(&actor_id("a")).unwrap();
    // teardown runs on the clean stop.
    probe.notify.notified().await;
    assert!(probe.teardown_called.load(Ordering::SeqCst));

    // Give the supervisor a chance to (wrongly) record a death, then assert it
    // did not: a clean stop is not a death and fires no death signal.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(health.died(), 0);
    assert!(dead.lock().unwrap().is_empty());
  }

  // ---- Scheduler actor (schedules a delayed message to itself) ----

  struct SchedulerProbe {
    fired: Arc<AtomicBool>,
    notify: Arc<Notify>,
    schedule: Arc<dyn Schedule>,
  }

  #[async_trait]
  impl Actor for SchedulerProbe {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
    async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
      match msg.type_.as_str() {
        "go" => self
          .schedule
          .schedule_self(std::time::Duration::from_millis(5), Message::empty("tick")),
        "tick" => {
          self.fired.store(true, Ordering::SeqCst);
          self.notify.notify_one();
        }
        _ => {}
      }
      Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
  }

  struct SchedulerCreator {
    fired: Arc<AtomicBool>,
    notify: Arc<Notify>,
  }

  impl ActorCreator for SchedulerCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(SchedulerProbe {
        fired: self.fired.clone(),
        notify: self.notify.clone(),
        schedule: caps.schedule(),
      }))
    }
  }

  #[tokio::test]
  async fn schedule_self_delivers_a_timer_message() {
    let fired = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let mut rt = Runtime::new();
    rt.register(
      "scheduler",
      SchedulerCreator {
        fired: fired.clone(),
        notify: notify.clone(),
      },
    );
    rt.spawn(actor_id("a"), "scheduler", &ActorConfig::default())
      .await
      .unwrap();

    // "go" makes the actor schedule a "tick" to itself; the timer delivers it
    // back into its own mailbox, where it's handled like any message.
    rt.deliver(&actor_id("a"), Message::empty("go"))
      .await
      .unwrap();
    notify.notified().await;

    assert!(fired.load(Ordering::SeqCst));
  }
}
