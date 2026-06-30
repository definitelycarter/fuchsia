use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorFactory, ActorId,
  ERROR_PORT, Emit, FailurePolicy, Message, MessageValue, OnError, OutputPorts,
};
use fuchsia_transport::{
  Ack, CorrelationId, DeadLetter, DeadLetterReason, DeadLettered, Delivery, Health, MailboxRx,
  MailboxTx, Outcome, mailbox,
};

use crate::error::RuntimeError;
use crate::registry::{ActorHandle, ActorRegistry};
use crate::schedule::TokioSchedule;
use crate::supervisor::{self, RestartControl};

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
    // A node that opted into restart (`max_restarts > 0`) needs a *rebuild
    // recipe* the supervisor keeps — the creator (a refcount-bumped `Arc`), so it
    // can re-`create` a crashed actor. A default node (`max_restarts == 0`) keeps
    // the lean slice-1 path and never builds a recipe, so it pays nothing here.
    // The lookup is the same name-keyed one `create`/`output_ports` just did, so
    // it cannot fail or drift; pulled now while `type_name` is in hand.
    let creator = if config.failure.restart.max_restarts > 0 {
      Some(self.factory.creator(type_name)?)
    } else {
      None
    };
    // The node's `emit` sink, pulled from the same capability bag the actor was
    // built from (`caps` is only *borrowed* by `create`, so it's still owned
    // here). The run loop holds its own handle so it can emit the error envelope
    // on the node's behalf under `OnError::RouteToError` — the actor never sees
    // this. A refcount bump of the `Arc<dyn Emit>` (falls back to a no-op sink
    // if the host granted none), exactly the threading slice 2 used for the
    // `FailurePolicy`.
    let emit = caps.emit();
    // The node's optional **dead-letter** sink, pulled from the same capability
    // bag. Unlike `emit`/`schedule` this is a *domain* capability the **product**
    // inserts under its own trait — `caps.insert::<dyn DeadLetter>(arc)` — so
    // most nodes have none (`None`), in which case the run loop keeps slice 2's
    // count-and-drop on `Health`. A refcount bump of the `Arc<dyn DeadLetter>`
    // when present, mirroring how `emit` is threaded for `RouteToError`.
    let dead_letter = caps.get::<dyn DeadLetter>();
    let ctx = Self::context(&actor_id);

    // The rebuild recipe ingredients, populated **only** for a restart-enabled
    // node; a default node carries `None`s and pays no extra clone. `caps` is
    // moved (it was only borrowed by `create`, so this is free); `config` is a
    // cold per-spawn clone, gated so a default node never pays it.
    let restart_enabled = creator.is_some();
    let recipe_caps = if restart_enabled { Some(caps) } else { None };
    let recipe_config = if restart_enabled {
      Some(config.clone())
    } else {
      None
    };

    Ok(Spawning {
      actor,
      ctx,
      actor_id,
      type_name: type_name.to_owned(),
      tx,
      rx,
      health,
      output_ports,
      // The host-understood failure policy for this node, read off its config so
      // the run loop can apply it around `handle`. A clone of a small value type
      // on the cold per-spawn path (no per-message cost); the run loop owns its
      // own copy because it outlives this `&config` borrow.
      failure: config.failure.clone(),
      emit,
      dead_letter,
      creator,
      caps: recipe_caps,
      config: recipe_config,
    })
  }

  /// Spawn a prepared (and set-up) actor's receive loop and register it as a
  /// routable target — the second half of a spawn. Re-checks for a duplicate id:
  /// another spawn may have committed the same id while `setup` ran outside the
  /// lock, in which case the prepared actor is dropped.
  ///
  /// Hands back a [`Committed`]: the node's mailbox/health, its declared
  /// [`OutputPorts`], and — **only** for a restart-enabled node — a
  /// [`RestartControl`] the engine keeps to drive `restart_node`.
  ///
  /// Two per-node task shapes, chosen by the restart budget:
  /// - `max_restarts == 0` (default): the lean slice-1 pair —
  ///   [`run_actor`] (moves in `rx`, unwinds on a `handle` panic) watched by
  ///   [`supervise`]. **No** new per-message cost.
  /// - `max_restarts > 0`: a single [`supervise_with_restart`] task that owns
  ///   `rx` + the rebuild recipe and catches `handle` panics so the mailbox
  ///   survives a crash.
  pub fn commit(&mut self, spawning: Spawning) -> Result<Committed, RuntimeError> {
    let Spawning {
      actor,
      ctx,
      actor_id,
      type_name,
      tx,
      rx,
      health,
      output_ports,
      failure,
      emit,
      dead_letter,
      creator,
      caps,
      config,
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
      type_name.clone(),
      // Refcount bump of the mpsc sender so the registry keeps a routable copy
      // while the caller (the engine) gets one for its router.
      tx.clone(),
      // Refcount bump of the shared health counters.
      health.clone(),
    );

    // Branch on the restart budget. A restart-enabled node (its `creator` /
    // `caps` / `config` recipe ingredients were populated by `prepare`) goes onto
    // the restart supervisor; everything else keeps the lean slice-1 pair, paying
    // nothing new.
    let restart = match (creator, caps, config) {
      (Some(creator), Some(caps), Some(config)) => {
        let control = supervisor::restart_control();
        let recipe = supervisor::RestartRecipe {
          creator,
          type_name,
          config,
          caps,
          ctx,
          // The supervisor needs an owned id for the recipe (run loop / dead
          // letters); a cold per-spawn `String` clone, like `ActorHandle::new`.
          node: actor_id.clone(),
          failure: failure.clone(),
          emit,
          dead_letter,
        };
        tokio::spawn(supervisor::supervise_with_restart(
          actor,
          recipe,
          rx,
          // A **weak** sender so a revived node can be re-registered without the
          // supervisor pinning `rx` open against a clean `stop`/`remove_graph`.
          tx.downgrade(),
          failure.restart.clone(),
          // Refcount bumps: the supervisor shares health + stop flag with the
          // registry handle.
          health.clone(),
          handle.stopping(),
          Arc::downgrade(&self.registry),
          self.death_listener.clone(),
          // Refcount bump (Arc inside) so the engine keeps the other half.
          control.clone(),
        ));
        Some(control)
      }
      // Default node: slice 1's pair, unchanged. Keep the actor's `JoinHandle`
      // and hand it to a per-node supervisor that turns its exit into a lifecycle
      // event — a panic / abnormal exit deregisters + records a death; a clean
      // stop does neither.
      _ => {
        let actor_task = tokio::spawn(run_actor(
          actor,
          ctx,
          rx,
          failure,
          emit,
          dead_letter,
          actor_id.clone(),
          // The node's shared health, so the poison gate's no-sink fallback can
          // bump the distinct poisoned counter. A refcount bump.
          health.clone(),
        ));
        tokio::spawn(supervise(
          actor_task,
          actor_id.clone(),
          health.clone(),
          handle.stopping(),
          // A **weak** handle to the registry, not a strong one: the registry
          // holds every node's mailbox sender, so a strong ref would keep all
          // those senders alive and a dropped `Runtime` could never close its
          // actors' mailboxes. Weak lets the registry drop with the `Runtime`.
          Arc::downgrade(&self.registry),
          self.death_listener.clone(),
        ));
        None
      }
    };

    registry.insert(handle);
    drop(registry);

    Ok(Committed {
      mailbox: tx,
      health,
      output_ports,
      restart,
    })
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
    // Direct callers (tests, a standalone runtime) want only the mailbox/health/
    // ports; the restart control is the engine's concern, so it is dropped here.
    // Dropping a restart-enabled node's control still leaves the supervisor task
    // running — `restart_node` is simply unavailable without going through the
    // engine, which is the supported path for it.
    let committed = self.commit(spawning)?;
    Ok((committed.mailbox, committed.health, committed.output_ports))
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

/// What [`Runtime::commit`] hands back once a node is running: its mailbox and
/// health (for the router + ingress), its declared [`OutputPorts`] (for edge
/// validation), and — **only** for a restart-enabled node — the
/// [`RestartControl`] the engine retains to drive `Engine::restart_node`. A
/// default node's `restart` is `None`.
pub struct Committed {
  pub mailbox: MailboxTx,
  pub health: Arc<Health>,
  pub output_ports: OutputPorts,
  /// The restart control handle for a restart-enabled node (`max_restarts > 0`);
  /// `None` for a default node, which cannot be force-restarted or revived.
  pub restart: Option<RestartControl>,
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
  /// The node's host-understood failure policy (continue / fail / retry /
  /// route-to-error), read off its config at [`Runtime::prepare`] and applied by
  /// [`run_actor`] around each `handle`. Unset = [`FailurePolicy::default`] =
  /// today's count + drop.
  failure: FailurePolicy,
  /// The node's `emit` sink, pulled from its capability bag at
  /// [`Runtime::prepare`]. The run loop holds it so it can emit an error
  /// envelope on the node's reserved `"error"` port under
  /// [`OnError::RouteToError`] — the runtime emits on the node's behalf, so this
  /// is the *runtime's* copy of the same sink the actor was granted.
  emit: Arc<dyn Emit>,
  /// The node's optional **dead-letter** sink, pulled from its capability bag at
  /// [`Runtime::prepare`]. A *domain* capability the product inserts (not one of
  /// fuchsia's universal `emit`/`schedule`), so it's `None` unless granted. When
  /// present, the run loop hands it the message that exhausts a `retry` budget
  /// or triggers a `fail` stop, instead of dropping + counting it. `None` keeps
  /// slice 2's count-and-drop behavior unchanged.
  dead_letter: Option<Arc<dyn DeadLetter>>,
  /// The node's creator, kept **only** for a restart-enabled node
  /// (`failure.restart.max_restarts > 0`) so its supervisor can rebuild a crashed
  /// actor. `None` for a default node, which keeps the lean slice-1 path. A
  /// refcount bump of the shared `Arc<dyn ActorCreator>`.
  creator: Option<Arc<dyn ActorCreator>>,
  /// The capability bag, kept **only** for a restart-enabled node so the
  /// supervisor can re-`create` from the *same* bag (re-offering the injected
  /// `schedule`/`emit`). Moved here (it was only borrowed by `create`), so it's
  /// free; `None` for a default node.
  caps: Option<ActorCapabilities>,
  /// The node's config, kept **only** for a restart-enabled node so the
  /// supervisor can re-`create` with it. A cold per-spawn clone gated to the
  /// restart path; `None` for a default node, which pays no clone.
  config: Option<ActorConfig>,
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

/// Where a failed delivery goes — the runtime-side destinations
/// [`handle_with_policy`] routes to, for *this* node, bundled so the policy
/// applier takes one borrow instead of three. Built once per `run_actor`, used
/// only on the cold error paths:
///
/// - `emit` — the node's `"error"` port sink for [`OnError::RouteToError`].
/// - `dead_letter` — the optional product-provided sink for an exhausted `retry`
///   / a `fail` stop; `None` falls back to count-and-drop.
/// - `node` — the failing node's id, stamped onto a [`DeadLettered`].
pub(crate) struct FailureSinks<'a> {
  pub(crate) emit: &'a dyn Emit,
  pub(crate) dead_letter: Option<&'a dyn DeadLetter>,
  pub(crate) node: &'a ActorId,
}

/// The poison-quarantine gate (**mechanism A**), run at the top of *every*
/// recv loop — the default [`run_actor`] and the restart supervisor's
/// `run_incarnation` — *before* a delivery reaches `handle`.
///
/// When `poison_after > 0` **and** the delivery's cross-delivery `attempts`
/// count *exceeds* it, the delivery is **diverted without being handled** (so a
/// poison message can't crash the node again): it is preserved on the
/// dead-letter sink (reason [`Poison`]) if one was granted, else counted on the
/// node's `Health` poisoned counter and dropped — and **`Ok` is reported on its
/// ack**, so an at-least-once feeder treats it as taken-responsibility-for and
/// stops re-delivering. Returns `None` (the caller does *not* call `handle`).
///
/// Otherwise (quarantine disabled, or the count is within budget) it returns
/// `Some(delivery)` for the caller to handle as normal — the common path, a bare
/// `u32` compare with no allocation and no clone.
///
/// [`Poison`]: fuchsia_transport::DeadLetterReason::Poison
pub(crate) fn poison_check(
  delivery: Delivery,
  poison_after: u32,
  sinks: &FailureSinks<'_>,
  health: &Health,
) -> Option<Delivery> {
  // Disabled (`0`) or within budget: hand the delivery straight back to be
  // handled. The hot path is a single compare on a `u32` already in hand.
  if poison_after == 0 || delivery.attempts <= poison_after {
    return Some(delivery);
  }

  // Over the threshold → quarantine. Divert without handling: move the message
  // to the sink (reason Poison) if one was granted, else count + drop on Health.
  let Delivery {
    msg,
    ack,
    correlation,
    attempts,
    ..
  } = delivery;
  // The quarantine is the event — fired whether or not a sink preserves it;
  // `dead_lettered` says which. Tagged with the run so a poison drop is traceable
  // to the request that caused it.
  tracing::warn!(
    correlation = %correlation,
    node = %sinks.node,
    attempts = attempts,
    dead_lettered = sinks.dead_letter.is_some(),
    "message.poisoned",
  );
  if let Some(sink) = sinks.dead_letter {
    record_dead_letter(
      sink,
      DeadLettered::new(
        msg,
        correlation,
        // Cold quarantine path — an owned id for the sink, like every other
        // dead-letter path. A real `String` clone, paid only on a poison divert.
        sinks.node.clone(),
        DeadLetterReason::Poison { attempts },
      ),
    );
  } else {
    // No sink: count the poison as a distinct outcome and drop the message. The
    // `Health` here is the *node's* shared counter (not the per-delivery ack), so
    // the drop is observable even for a `Complete`-acked delivery.
    health.record_poison();
  }
  // Report `Ok` so the feeder stops re-delivering: the runtime has taken
  // responsibility for the poison, so an `Ack::Complete` durable caller must not
  // retry (and re-poison) it, and a `Health` ack is satisfied. The message was
  // *not* handled — this is the quarantine outcome, not a handle result.
  ack.report(Ok(()));
  None
}

/// Dead-letter one message: fire the `dead_letter` event (so the transition is
/// visible in a trace, grouped by its run) **and** hand it to the sink. The one
/// path every dead-letter goes through, so the event can never drift from the
/// sink call. WARN — a dead letter is always a message the runtime gave up on.
pub(crate) fn record_dead_letter(sink: &dyn DeadLetter, letter: DeadLettered) {
  tracing::warn!(
    correlation = %letter.correlation,
    node = %letter.node,
    reason = letter.reason.label(),
    "dead_letter",
  );
  sink.dead_letter(letter);
}

// The default (non-restart) recv loop's ingredients — the actor, its mailbox,
// the failure policy + sinks, and the shared health for the poison gate's
// no-sink fallback. One per-node task, built once at `commit`; the arg count is
// the cost of keeping the lean path free of a per-message struct build.
#[allow(clippy::too_many_arguments)]
async fn run_actor(
  mut actor: Box<dyn Actor>,
  ctx: ActorContext,
  mut rx: MailboxRx,
  failure: FailurePolicy,
  emit: Arc<dyn Emit>,
  dead_letter: Option<Arc<dyn DeadLetter>>,
  node: ActorId,
  health: Arc<Health>,
) {
  // Borrow the failure destinations once for the whole loop — the `Arc`s /
  // `Option` live for `run_actor`, so this is a single bundle of references the
  // policy applier reuses per delivery (no per-message rebuild).
  let sinks = FailureSinks {
    emit: emit.as_ref(),
    dead_letter: dead_letter.as_deref(),
    node: &node,
  };

  while let Some(delivery) = rx.recv().await {
    // Mechanism A: quarantine a poison delivery (attempts over `poison_after`)
    // before it reaches `handle`, so it can't crash the node again. `None` means
    // it was diverted (sink/Health) + `Ok`-acked here; skip to the next message.
    let Some(delivery) = poison_check(delivery, failure.poison_after, &sinks, &health) else {
      continue;
    };
    let Delivery {
      msg,
      ack,
      span: parent,
      correlation,
      ..
    } = delivery;

    // Apply the node's failure policy around `handle`. Returns the final outcome
    // (after any retries) and whether the policy says to **stop** the node. The
    // `sinks` carry where a failure goes: the `"error"` port (RouteToError), and
    // the optional dead-letter sink for an exhausted `retry` / a `fail` stop.
    let (outcome, stop) = handle_with_policy(
      &mut actor,
      &ctx,
      &failure.on_error,
      msg,
      &parent,
      correlation,
      &sinks,
    )
    .await;
    ack.report(outcome);

    if stop {
      // `OnError::Fail`: break the loop *without* any stop flag set, so the
      // per-node supervisor classifies this clean exit as a **death** — it
      // records `Health::died`, deregisters the node from the registry, and
      // fires the `DeathListener` so the engine drops it from its router. We
      // reuse slice 1's death machinery rather than adding a parallel
      // teardown/deregister path. `teardown` still runs below before we return,
      // exactly as on a normal loop exit.
      break;
    }
  }

  let _ = actor.teardown(&ctx).await;
}

/// Run one delivery through `handle`, applying the node's [`OnError`] policy.
///
/// Returns `(final_outcome, stop)`: the outcome to report on the `Ack` **once**
/// (so an `Ack::Complete` durable caller sees the final result and `Health`
/// reflects the final state), and whether the node should stop (`OnError::Fail`
/// on an error).
///
/// Performance: the common path — `Continue`, a `Fail`/`Retry` that succeeds, or
/// any policy with **no dead-letter sink** — moves `msg` straight into `handle`
/// with **no clone and no extra allocation**, exactly as before. A `Message` is
/// cloned (it can deep-copy JSON/bytes, so the clone is kept off every other
/// path) *only*: (1) on the retry path between attempts when one errored and
/// another will follow, and (2) to *preserve* the original for a dead-letter
/// sink that is present — on a `fail` stop, or an exhausted `retry` — so the
/// sink receives the message `handle` is about to consume. Both are cold failure
/// paths; the no-sink fallback pays neither.
///
/// Ack semantics — one rule across the arms: the returned `Outcome` is **`Ok`**
/// when the runtime *quarantined* the message on a **surviving** node (diverted
/// it to the error port under `RouteToError`, or dead-lettered an exhausted
/// `retry`), since it is no longer a retriable failure and an at-least-once
/// durable caller must not retry and double-handle it. It is the real **`Err`**
/// when the node **dies** (`Fail`) or nothing took responsibility (no sink →
/// count-and-drop). A `fail` that dies on a message still dead-letters it to
/// preserve it, but reports `Err` — it must not tell a durable caller "success"
/// when the node just crashed on their input. Survive-and-quarantine → `Ok`;
/// die-or-drop → the real outcome.
pub(crate) async fn handle_with_policy(
  actor: &mut Box<dyn Actor>,
  ctx: &ActorContext,
  on_error: &OnError,
  msg: Message,
  parent: &tracing::Span,
  correlation: CorrelationId,
  sinks: &FailureSinks<'_>,
) -> (Outcome, bool) {
  let FailureSinks {
    emit,
    dead_letter,
    node,
  } = *sinks;
  match on_error {
    // Today's behavior: one attempt, fold the outcome into `Health` + drop on
    // error, keep going. `correlation` is *moved* in (no clone), no stop.
    OnError::Continue => {
      let outcome = handle_once(actor, ctx, msg, parent, correlation).await;
      (outcome, false)
    }
    // Fail-fast: one attempt; on error, signal the caller to stop the node
    // (death path). The errored outcome is still reported on the ack.
    //
    // Dead-letter is **additive** on `fail`: when a sink is present, the
    // triggering message is preserved (reason `Failed`) *before* the node stops,
    // but the ack still reports the original `Err` and the supervisor still
    // records `died` — slice 1/2's death/ack/stop behavior is unchanged. The
    // node is dying regardless, so the dead-letter preserves the message rather
    // than replacing the failure signal (the durable feeder remains free to do
    // its own thing; re-delivery just hits a now-deregistered node).
    OnError::Fail => {
      // The message is consumed by `handle_once`; snapshot it *only* when a sink
      // is present (so the cold fail path can dead-letter the original), keeping
      // the clone off the no-sink path entirely. A `Message` clone can deep-copy
      // its JSON/bytes, so it is paid only here, on a node that is about to die.
      let preserved = dead_letter.map(|_| msg.clone());
      let outcome = handle_once(actor, ctx, msg, parent, correlation.clone()).await;
      let stop = outcome.is_err();
      if stop && let (Some(sink), Some(preserved)) = (dead_letter, preserved) {
        let error = match &outcome {
          Err(err) => err.to_string(),
          Ok(()) => String::new(),
        };
        record_dead_letter(
          sink,
          DeadLettered::new(
            preserved,
            correlation,
            // Cold fail path on a dying node — an owned id for the sink. A real
            // `String` clone, but paid once, only when a node fails with a sink.
            node.clone(),
            DeadLetterReason::Failed { error },
          ),
        );
      }
      (outcome, stop)
    }
    // Retry the *same* message up to `max` times after the first failure, with
    // backoff between attempts. The ack reports the **final** outcome once.
    //
    // The first `max` attempts run in the loop below, each on a *clone* of the
    // message (so it survives for a possible retry); the final, `(max + 1)`-th
    // attempt runs *after* the loop and **moves** `msg` + `correlation` in (no
    // clone — nothing survives it). Keeping the final move out of the loop is
    // what lets the borrow checker see `msg`/`correlation` consumed exactly
    // once. With `max == 0` the loop body never runs, so this reduces to a
    // single, clone-free attempt — identical to `Continue`.
    OnError::Retry { max, backoff } => {
      for attempt in 0..*max {
        // A retry may follow, so the message and correlation must survive this
        // attempt. The `Message` clone is the documented retry-path clone
        // (AGENTS.md): preserving the *delivered* message across a
        // re-invocation is the whole point of `retry`, and `Message: Clone`
        // exists for it. It is paid *only* here — never on the success path,
        // the final attempt, or any non-retry policy. `correlation.clone()` is
        // a refcount bump of its inner `Arc<str>`.
        let retry_copy = msg.clone();
        let outcome = handle_once(actor, ctx, retry_copy, parent, correlation.clone()).await;
        if outcome.is_ok() {
          // Succeeded within budget — done; the surviving `msg` is dropped.
          return (outcome, false);
        }
        // Errored, a retry will follow: surface it (the failed attempt is
        // `attempt + 1`, since the loop is 0-based). No `actor.handle` span is
        // active here — it lives inside `handle_once` — so the event carries the
        // correlation explicitly.
        tracing::debug!(
          correlation = %correlation,
          node = %node,
          attempt = attempt + 1,
          "handle.retry",
        );
        // Errored with attempts remaining: wait the backoff, then retry.
        let delay = backoff.delay_for(attempt);
        if !delay.is_zero() {
          tokio::time::sleep(delay).await;
        }
      }

      // Final, `(max + 1)`-th attempt. Total attempts on exhaustion = `max + 1`
      // (one initial + `max` retries).
      let attempts = *max + 1;
      // Snapshot the message *only* when a dead-letter sink is present, so an
      // exhausted retry can preserve the original; with no sink this is `None`
      // and the message is moved straight into the final `handle_once` — the
      // count-and-drop path keeps slice 2's behavior with zero extra clone.
      // A `Message` clone can deep-copy its JSON/bytes, so it is paid only on the
      // sink-present exhausted-retry path, never on success or with no sink.
      let preserved = dead_letter.map(|_| msg.clone());
      let outcome = handle_once(actor, ctx, msg, parent, correlation.clone()).await;
      match (&outcome, dead_letter, preserved) {
        // Exhausted retry *and* a sink is present: hand it the message (reason
        // `RetryExhausted`) instead of dropping it. The runtime has taken
        // responsibility, so this is **not** a retriable failure — report `Ok`,
        // so an at-least-once `Ack::Complete` durable caller doesn't retry and
        // produce a *duplicate* dead-letter. The node stays alive.
        (Err(err), Some(sink), Some(preserved)) => {
          let error = err.to_string();
          record_dead_letter(
            sink,
            DeadLettered::new(
              preserved,
              correlation,
              // Cold exhausted-retry path — an owned id for the sink. A real
              // `String` clone, paid once, only when a retry exhausts with a sink.
              node.clone(),
              DeadLetterReason::RetryExhausted { attempts, error },
            ),
          );
          (Ok(()), false)
        }
        // No sink (or the final attempt unexpectedly succeeded): slice 2's
        // fallback — report the (possibly errored) outcome so `Health` counts it
        // and the message is dropped, keeping the node alive for the next
        // message. The durable caller remains the retry-of-last-resort.
        _ => (outcome, false),
      }
    }
    // Divert a handled `Err` to the node's reserved `"error"` output port, then
    // keep going (no stop). One attempt; on success this is identical to
    // `Continue`.
    //
    // On error the runtime builds an **error envelope** — the error string, the
    // node id, and the *original* message's type + payload — and emits it on
    // `"error"` on the node's behalf. The envelope is stamped with **this
    // delivery's** correlation (`sync_scope`), so the right run's error branch
    // fires even though `handle`'s own scope has already ended. If nothing is
    // wired to `"error"`, the engine counts the emit as `no_route` on
    // `(node, "error")` — the honest fallback until the dead-letter sink (part 4).
    //
    // Ack semantics: the diverted error is reported as **`Ok(())`**, not the
    // original `Err`. The failure has been routed to the error branch, so it is
    // *not* a retriable failure — reporting `Err` would make an at-least-once
    // `Ack::Complete` durable caller (`push_durable`) retry and **double-process**
    // (the error branch would fire a second time). The node continues either way.
    OnError::RouteToError => {
      // Snapshot the original type + payload *before* `handle` consumes `msg`;
      // needed only if `handle` errors. This clone is on the cold error-handling
      // path's setup but unavoidable — `handle_once` moves `msg` — so it is kept
      // to the `RouteToError` arm only, never the success/other policies. The
      // payload clone (a JSON/bytes deep-copy) is only *used* on the error path
      // (it builds the envelope); on success the snapshot is simply dropped.
      let type_ = msg.type_.clone();
      let payload = msg.value.clone();
      // `correlation` is moved into `handle_once`; keep a refcount-bumped copy to
      // stamp the envelope emit after the handle scope ends.
      let outcome = handle_once(actor, ctx, msg, parent, correlation.clone()).await;
      if let Err(err) = &outcome {
        let envelope = error_envelope(&err.to_string(), &ctx.node_id, &type_, payload);
        // Emit *within this delivery's correlation scope* so `RoutedEmit`'s
        // `Delivery::new` stamps the error branch with the triggering run's id —
        // `handle`'s own scope is already gone by here. Synchronous, since
        // `Emit::emit_to` is a sync call.
        correlation.sync_scope(|| emit.emit_to(ERROR_PORT, envelope));
      }
      // Diverted, not retriable: report `Ok` (see the ack-semantics note above).
      (Ok(()), false)
    }
    // `OnError` is `#[non_exhaustive]`: a *future* variant (e.g. the dead-letter
    // terminal action, part 4) is not handled here. Until its slice lands, fall
    // back to `Continue` semantics — one attempt, count + drop on error, node
    // stays alive — rather than failing to compile or stopping the node.
    _ => {
      let outcome = handle_once(actor, ctx, msg, parent, correlation).await;
      (outcome, false)
    }
  }
}

/// Build the error envelope emitted on the reserved `"error"` port under
/// [`OnError::RouteToError`]: a JSON message carrying the error string, the
/// originating node id, and the *original* message's type + payload, so the
/// error sub-graph has everything it needs to react.
///
/// The envelope's own `type_` is `"error"`. The original payload maps by
/// [`MessageValue`] variant: `Json(v)` embeds `v` verbatim; `Empty` → JSON
/// `null`; `Binary` → a `{ "byte_len": <len> }` marker rather than the bytes
/// themselves — the envelope is a JSON message, so embedding raw bytes would
/// force an encoding choice (base64) and a new dependency. The marker keeps the
/// envelope honest (the error branch sees that an N-byte binary payload
/// triggered the failure); naming the field `byte_len` (not `bytes`) leaves
/// `bytes` free to carry a base64 of the content later — binary logging / UI
/// inspection — without a breaking rename.
fn error_envelope(error: &str, node: &str, type_: &str, payload: MessageValue) -> Message {
  let payload_json = match payload {
    MessageValue::Json(v) => v,
    MessageValue::Empty => serde_json::Value::Null,
    MessageValue::Binary(bytes) => serde_json::json!({ "byte_len": bytes.len() }),
  };
  Message::json(
    "error",
    serde_json::json!({
      "error": error,
      "node": node,
      "type": type_,
      "payload": payload_json,
    }),
  )
}

/// One `handle` invocation, fully instrumented: builds the per-delivery
/// [`ActorContext`], enters the correlation scope + trace span for the duration
/// of the async handle, and returns its outcome. Pulled out of [`run_actor`] so
/// the retry loop can re-invoke it on the same message without duplicating the
/// per-attempt context/span setup.
async fn handle_once(
  actor: &mut Box<dyn Actor>,
  ctx: &ActorContext,
  msg: Message,
  parent: &tracing::Span,
  correlation: fuchsia_transport::CorrelationId,
) -> Outcome {
  use tracing::Instrument;
  // The handle span is a child of the upstream's span (carried on the
  // delivery), so a trace follows the message across this mailbox hop. The
  // actor's own emits, made inside this span, propagate it onward. DEBUG so
  // it's off the hot path unless tracing is turned up. A fresh span per attempt
  // so each retry is its own traced handling.
  //
  // `correlation` is recorded as a field (not just inherited via the parent
  // chain), so a subscriber can group every span and event by run id — the
  // whole point of the correlation. It is an `Arc<str>` display, no allocation.
  // `outcome` starts empty and is filled once the handle resolves (below).
  let span = tracing::debug_span!(
    parent: parent,
    "actor.handle",
    correlation = %correlation,
    node = %ctx.node_id,
    kind = %msg.type_,
    outcome = tracing::field::Empty,
  );

  // Build a **per-delivery** context, giving the three id fields distinct
  // meanings: `node_id` static (which actor — the stable spawn-time id),
  // `execution_id` the run this message belongs to (the delivery's
  // correlation), `task_id` this handling (a fresh per-attempt id).
  //
  // Both shared ids are `Arc<str>` refcount bumps, not allocations:
  // `execution_id` is the correlation's inner arc, and `node_id` is an
  // `Arc::clone` of the actor's stable id. `task_id` is a bare `u64` counter —
  // no allocation either; the `"task-N"` string is rendered lazily, only if a
  // guest host reads it (`ActorContext::task_label`). So the per-message context
  // build allocates nothing.
  let execution_id = correlation.as_arc(); // refcount bump
  let node_id = Arc::clone(&ctx.node_id); // refcount bump of the stable spawn-time id
  let msg_ctx = ActorContext::new(execution_id, node_id, next_task_id());

  // Enter the correlation for the handle — a task-local mirroring the span, so
  // emits the actor makes inside `handle` capture this run id and propagate it
  // onward. `.instrument(span).await` enters the span for the duration of the
  // async handle without holding a `!Send` span guard across the await point.
  // Recording `outcome` from *inside* the instrumented future
  // (`Span::current()` is the entered handle span there) fills the field on
  // close without cloning the span.
  correlation
    .scope(
      async move {
        let outcome = actor.handle(&msg_ctx, msg).await;
        tracing::Span::current().record(
          "outcome",
          match &outcome {
            Ok(()) => "ok",
            Err(_) => "error",
          },
        );
        outcome
      }
      .instrument(span),
    )
    .await
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

  // A pure node death — the task is gone, so there is no in-flight message and
  // the event is node-keyed (no correlation). The `NodeDied` drain below
  // re-attaches a correlation per *bystander* message it dead-letters.
  if let Err(join_err) = &join {
    // The panic was swallowed before (the `JoinHandle` was discarded); surface
    // it so a dead node is not silent.
    tracing::error!(node = %actor_id, cause = "panic", error = %join_err, "node.died");
  } else {
    tracing::warn!(node = %actor_id, cause = "abnormal_exit", "node.died");
  }

  record_death(
    &actor_id,
    &health,
    registry.as_ref(),
    death_listener.as_ref(),
  );
}

/// Record a node's permanent death and tear down its addressing: bump the
/// distinct `died` counter on [`Health`], deregister it from the runtime's
/// [`ActorRegistry`] so it stops resolving for `deliver`, and fire the
/// [`DeathListener`] so the engine drops it from its router. Shared by the
/// non-restart [`supervise`] path and the restart supervisor's
/// permanent-death (budget-exhausted) path, so a death looks identical however
/// it is reached.
///
/// `registry` is the *upgraded* strong handle (or `None` if the whole runtime
/// has already been dropped). Best-effort on a poisoned lock — the death is
/// already on `Health`, and a poisoned registry means the process is already
/// unwinding.
pub(crate) fn record_death(
  actor_id: &ActorId,
  health: &Health,
  registry: Option<&Arc<Mutex<ActorRegistry>>>,
  death_listener: Option<&DeathListener>,
) {
  // Observable as a distinct death on the node's shared `Health` (the `died`
  // counter, not `errored`).
  health.record_death();

  // Deregister from the runtime's address book so the node stops resolving for
  // `deliver`.
  if let Some(registry) = registry
    && let Ok(mut registry) = registry.lock()
  {
    registry.remove(actor_id);
  }

  // Tell the layer above (the engine) so it drops the node from its router,
  // where routed deliveries actually resolve. Runs last so the runtime's own
  // state is consistent first.
  if let Some(listener) = death_listener {
    listener(actor_id);
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
  async fn transient_restart_is_observable_as_a_crash_not_a_death() {
    use fuchsia_actor::{Backoff, FailurePolicy};
    use std::time::Duration;
    // A restart-enabled node whose `handle` panics is caught and rebuilt (a
    // *transient* restart). The crash must be observable on `Health::crashed`,
    // accounting for the dropped in-flight delivery, while `died` stays 0 — the
    // node lived on. Without this counter a flapping node would look healthy.
    let mut rt = Runtime::new();
    rt.register("panic", PanicCreator);
    let (dead, _notify) = record_deaths(&mut rt);

    let config = ActorConfig {
      failure: FailurePolicy::restart(3, Backoff::fixed(Duration::from_millis(1))),
      ..Default::default()
    };
    let (tx, health, _ports) = rt
      .spawn_with_caps(actor_id("a"), "panic", &config, ActorCapabilities::new())
      .await
      .unwrap();
    drop(tx);

    // Each delivered message is one caught panic → one transient rebuild, and
    // `crashed` rises with each — a flapping node is visible even while it serves.
    for n in 1u64..=2 {
      rt.deliver(&actor_id("a"), Message::empty("boom"))
        .await
        .unwrap();
      let mut ok = false;
      for _ in 0..200 {
        if health.crashed() == n {
          ok = true;
          break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
      assert!(ok, "transient restart #{n} must bump Health::crashed");
      // Each is a transient rebuild, not a death — the node lives on.
      assert_eq!(health.died(), 0, "a transient rebuild is not a death");
      assert!(
        dead.lock().unwrap().is_empty(),
        "no death listener fires on a rebuild"
      );
      assert!(
        rt.registry_contains(&actor_id("a")).unwrap(),
        "the node rebuilt and still resolves"
      );
    }
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

  // ---- Failure-policy actor (configurable per-message Err/Ok) -----------------

  use fuchsia_actor::{Backoff, FailurePolicy};
  use std::sync::atomic::AtomicU64;
  use std::time::{Duration, Instant};

  /// Observes a node that errors a configurable number of times.
  ///
  /// `fail_first` = how many of its *first* `handle` invocations return `Err`
  /// (so `u32::MAX` = always error); `calls` counts every `handle` invocation;
  /// `teardown_called` flips when `teardown` runs.
  struct FailProbe {
    fail_first: u32,
    calls: AtomicU64,
    teardown_called: AtomicBool,
    notify: Notify,
  }

  impl FailProbe {
    fn new(fail_first: u32) -> Arc<Self> {
      Arc::new(Self {
        fail_first,
        calls: AtomicU64::new(0),
        teardown_called: AtomicBool::new(false),
        notify: Notify::new(),
      })
    }
  }

  struct FailActor {
    probe: Arc<FailProbe>,
  }

  #[async_trait]
  impl Actor for FailActor {
    async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
      // 1-indexed count of this invocation.
      let n = self.probe.calls.fetch_add(1, Ordering::SeqCst) + 1;
      self.probe.notify.notify_one();
      if (n as u32) <= self.probe.fail_first {
        Err(ActorError::Handle("intentional".to_owned()))
      } else {
        Ok(())
      }
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      self.probe.teardown_called.store(true, Ordering::SeqCst);
      self.probe.notify.notify_one();
      Ok(())
    }
  }

  struct FailCreator {
    probe: Arc<FailProbe>,
  }

  impl ActorCreator for FailCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(FailActor {
        probe: self.probe.clone(),
      }))
    }
  }

  /// Spawn a `FailActor` under `policy`, returning its handle bits + probe.
  /// Drops the caller's `tx` so the registry's sender is the only one keeping
  /// the mailbox open (matching how a real spawner hands the tx onward).
  async fn spawn_failing(
    rt: &mut Runtime,
    id: &str,
    fail_first: u32,
    policy: FailurePolicy,
  ) -> (Arc<Health>, Arc<FailProbe>) {
    let probe = FailProbe::new(fail_first);
    rt.register(
      "fail",
      FailCreator {
        probe: probe.clone(),
      },
    );
    let config = ActorConfig {
      failure: policy,
      ..Default::default()
    };
    let (tx, health, _ports) = rt
      .spawn_with_caps(actor_id(id), "fail", &config, ActorCapabilities::new())
      .await
      .unwrap();
    drop(tx);
    (health, probe)
  }

  #[tokio::test]
  async fn continue_policy_counts_error_drops_and_keeps_going() {
    // Default policy = continue. Error once, then succeed; the node survives the
    // error and handles the next message.
    let mut rt = Runtime::new();
    let (health, probe) = spawn_failing(&mut rt, "a", 1, FailurePolicy::default()).await;

    // First message errors: counted on Health, dropped, node stays alive.
    rt.deliver(&actor_id("a"), Message::empty("one"))
      .await
      .unwrap();
    probe.notify.notified().await;

    // Second message succeeds — proving the node kept handling after the error.
    rt.deliver(&actor_id("a"), Message::empty("two"))
      .await
      .unwrap();
    // Wait until both calls have landed.
    while probe.calls.load(Ordering::SeqCst) < 2 {
      probe.notify.notified().await;
    }

    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    assert_eq!(health.handled(), 2);
    assert_eq!(health.errored(), 1);
    assert_eq!(health.died(), 0);
    // Still resolves — it was not stopped.
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn retry_succeeds_within_budget() {
    // Error twice then succeed, with max=3 retries: the third *attempt* (second
    // retry) handles OK. handle is invoked exactly 3 times; the final outcome is
    // OK so errored stays 0 and the message counts as handled once.
    let mut rt = Runtime::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (health, probe) = spawn_failing(&mut rt, "a", 2, FailurePolicy::retry(3, backoff)).await;

    rt.deliver(&actor_id("a"), Message::empty("go"))
      .await
      .unwrap();
    while probe.calls.load(Ordering::SeqCst) < 3 {
      probe.notify.notified().await;
    }
    // Let the ack land after the final (successful) attempt.
    tokio::time::sleep(Duration::from_millis(20)).await;

    // 1 initial + 2 retries = 3 invocations, ending OK.
    assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
    // The ack reports the *final* outcome once: handled, not errored.
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 0);
    assert_eq!(health.died(), 0);
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn retry_exhausted_drops_counts_and_stays_alive() {
    // Always errors; max=2 retries → 3 attempts, all fail. Slice-2 fallback:
    // count + drop (the final errored outcome), node stays alive for the next
    // message.
    let mut rt = Runtime::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (health, probe) =
      spawn_failing(&mut rt, "a", u32::MAX, FailurePolicy::retry(2, backoff)).await;

    rt.deliver(&actor_id("a"), Message::empty("go"))
      .await
      .unwrap();
    while probe.calls.load(Ordering::SeqCst) < 3 {
      probe.notify.notified().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;

    // 1 initial + 2 retries = 3 attempts, all errored.
    assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
    // Reported once: a single errored outcome (count + drop), no death.
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 1);
    assert_eq!(health.died(), 0);
    // Node survives an exhausted retry — still resolves and handles the next.
    assert!(rt.registry_contains(&actor_id("a")).unwrap());

    rt.deliver(&actor_id("a"), Message::empty("again"))
      .await
      .unwrap();
    while probe.calls.load(Ordering::SeqCst) < 6 {
      probe.notify.notified().await;
    }
    assert_eq!(probe.calls.load(Ordering::SeqCst), 6);
  }

  #[tokio::test]
  async fn fail_policy_stops_the_node_as_a_death() {
    // Under `fail`, an errored handle stops the node via slice 1's death path:
    // teardown runs, Health records a death, and it deregisters from the
    // registry + fires the death listener (the engine's router seam).
    let mut rt = Runtime::new();
    let (dead, notify) = record_deaths(&mut rt);
    let (health, probe) = spawn_failing(&mut rt, "a", u32::MAX, FailurePolicy::fail()).await;

    rt.deliver(&actor_id("a"), Message::empty("boom"))
      .await
      .unwrap();

    // The death signal fires once the failing handle breaks the loop and
    // teardown runs.
    notify.notified().await;

    // teardown ran (clean break still tears down), and exactly one handle call.
    assert!(probe.teardown_called.load(Ordering::SeqCst));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);

    // The errored outcome is still reported once, *and* the death is recorded as
    // a distinct event (slice 1's `died`, not folded into `errored`).
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 1);
    assert_eq!(health.died(), 1);

    // The listener was told which node died.
    assert_eq!(dead.lock().unwrap().as_slice(), &[actor_id("a")]);

    // The node stops resolving: deregistered from the registry, and deliver now
    // reports it gone.
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());
    let err = rt
      .deliver(&actor_id("a"), Message::empty("again"))
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::ActorNotFound(_)));
  }

  #[tokio::test]
  async fn retry_waits_the_backoff_between_attempts() {
    // Loose, non-flaky timing assertion: always-error with max=2 and a 30ms
    // fixed backoff must take *at least* ~2 backoffs (≈60ms) before the final
    // outcome lands. We assert a conservative lower bound only.
    let mut rt = Runtime::new();
    let backoff = Backoff::fixed(Duration::from_millis(30));
    let (health, probe) =
      spawn_failing(&mut rt, "a", u32::MAX, FailurePolicy::retry(2, backoff)).await;

    let start = Instant::now();
    rt.deliver(&actor_id("a"), Message::empty("go"))
      .await
      .unwrap();
    // Wait until the final outcome has been reported (Health folds it in).
    while health.handled() == 0 {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let elapsed = start.elapsed();

    assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
    // Two backoffs of 30ms between the three attempts → at least ~50ms even with
    // scheduling slack. Lower bound only, so it stays non-flaky.
    assert!(
      elapsed >= Duration::from_millis(50),
      "expected backoff delay, took {elapsed:?}"
    );
  }

  // ---- dead-letter sink (part 4) ----------------------------------------------

  use fuchsia_transport::{DeadLetter, DeadLetterReason, DeadLettered};

  /// A recording `DeadLetter` sink — the shape a product's real impl takes, minus
  /// the store. Records every dead letter and notifies, so a test can await the
  /// dead-letter signal rather than sleep.
  struct RecorderDeadLetter {
    letters: Mutex<Vec<DeadLettered>>,
    notify: Notify,
  }

  impl RecorderDeadLetter {
    fn new() -> Arc<Self> {
      Arc::new(Self {
        letters: Mutex::new(Vec::new()),
        notify: Notify::new(),
      })
    }
  }

  impl DeadLetter for RecorderDeadLetter {
    fn dead_letter(&self, letter: DeadLettered) {
      self.letters.lock().unwrap().push(letter);
      self.notify.notify_one();
    }
  }

  /// Spawn a `FailActor` under `policy` with `sink` inserted as its `DeadLetter`
  /// capability — a *domain* capability the product inserts under its own trait,
  /// exactly as a real product would (`caps.insert::<dyn DeadLetter>(arc)`).
  async fn spawn_failing_with_dead_letter(
    rt: &mut Runtime,
    id: &str,
    fail_first: u32,
    policy: FailurePolicy,
    sink: Arc<RecorderDeadLetter>,
  ) -> (Arc<Health>, Arc<FailProbe>) {
    let probe = FailProbe::new(fail_first);
    rt.register(
      "fail",
      FailCreator {
        probe: probe.clone(),
      },
    );
    let config = ActorConfig {
      failure: policy,
      ..Default::default()
    };
    let mut caps = ActorCapabilities::new();
    // Insert under the trait-object type, the way a product grants a domain
    // capability — there is no `with_dead_letter` helper, just the generic seam.
    caps.insert::<dyn DeadLetter>(sink);
    let (tx, health, _ports) = rt
      .spawn_with_caps(actor_id(id), "fail", &config, caps)
      .await
      .unwrap();
    drop(tx);
    (health, probe)
  }

  #[tokio::test]
  async fn exhausted_retry_dead_letters_and_acks_ok() {
    // Always errors; max=2 retries → 3 attempts, all fail. With a sink present,
    // the exhausted retry hands the original message to the dead-letter sink
    // (reason RetryExhausted, attempts=3) instead of dropping+counting it, the
    // ack reports Ok (so Health counts it handled, not errored), and the node
    // survives for the next message.
    let mut rt = Runtime::new();
    let sink = RecorderDeadLetter::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (health, probe) = spawn_failing_with_dead_letter(
      &mut rt,
      "a",
      u32::MAX,
      FailurePolicy::retry(2, backoff),
      sink.clone(),
    )
    .await;

    rt.deliver(
      &actor_id("a"),
      Message::json("reading", serde_json::json!({ "v": 1 })),
    )
    .await
    .unwrap();
    sink.notify.notified().await;

    // Exactly one dead letter, carrying the original message + node + reason.
    {
      let letters = sink.letters.lock().unwrap();
      assert_eq!(letters.len(), 1);
      let letter = &letters[0];
      assert_eq!(letter.msg.type_, "reading");
      assert_eq!(
        letter.msg.value,
        MessageValue::Json(serde_json::json!({ "v": 1 }))
      );
      assert_eq!(letter.node, actor_id("a"));
      assert_eq!(
        letter.reason,
        DeadLetterReason::RetryExhausted {
          attempts: 3, // 1 initial + 2 retries
          error: "handle failed: intentional".to_owned(),
        }
      );
    }

    // 1 initial + 2 retries = 3 attempts, all errored.
    assert_eq!(probe.calls.load(Ordering::SeqCst), 3);
    // Dead-lettered, not a retriable failure: the ack reports Ok, so Health
    // counts it handled (not errored) and the node is not dead.
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 0);
    assert_eq!(health.died(), 0);

    // The node survives — a second message is still handled.
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
    rt.deliver(&actor_id("a"), Message::empty("again"))
      .await
      .unwrap();
    while probe.calls.load(Ordering::SeqCst) < 6 {
      probe.notify.notified().await;
    }
    assert_eq!(probe.calls.load(Ordering::SeqCst), 6);
  }

  #[tokio::test]
  async fn exhausted_retry_no_sink_counts_and_drops() {
    // Regression guard: with *no* dead-letter sink, an exhausted retry keeps
    // slice-2 behavior exactly — count + drop the final errored outcome on
    // Health, node stays alive. (The full assertion is in
    // `retry_exhausted_drops_counts_and_stays_alive`; this asserts the errored
    // ack specifically, the thing the sink path flips to Ok.)
    let mut rt = Runtime::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (health, _probe) =
      spawn_failing(&mut rt, "a", u32::MAX, FailurePolicy::retry(2, backoff)).await;

    rt.deliver(&actor_id("a"), Message::empty("go"))
      .await
      .unwrap();
    // Wait for the final (errored) outcome to land on Health.
    while health.handled() == 0 {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // No sink → the exhausted retry reports the final Err: errored, not handled-Ok.
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 1);
    assert_eq!(health.died(), 0);
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn exhausted_retry_no_sink_durable_ack_reports_err() {
    // Engine-free durable-ack regression: with no sink, an exhausted retry must
    // still report the final Err through an `Ack::Complete`, so a durable caller
    // (`push_durable`) sees the failure and remains the retry-of-last-resort.
    let mut rt = Runtime::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (_health, _probe) =
      spawn_failing(&mut rt, "a", u32::MAX, FailurePolicy::retry(2, backoff)).await;

    // Send a delivery carrying a Complete ack (the durable path) and assert the
    // outcome that comes back is the final Err.
    let mailbox = {
      let registry = rt.registry.lock().unwrap();
      registry.get(&actor_id("a")).unwrap().mailbox().clone()
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let delivery = Delivery::new(Message::empty("go"), Ack::Complete(tx));
    assert!(mailbox.send(delivery).await.is_ok());

    let outcome = rx.await.unwrap();
    assert!(outcome.is_err(), "exhausted retry with no sink reports Err");
  }

  #[tokio::test]
  async fn exhausted_retry_with_sink_durable_ack_reports_ok() {
    // The other half of the ack-semantics decision: *with* a sink, an exhausted
    // retry reports Ok through the Complete ack, so an at-least-once durable
    // caller doesn't retry and produce a duplicate dead-letter.
    let mut rt = Runtime::new();
    let sink = RecorderDeadLetter::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (_health, _probe) = spawn_failing_with_dead_letter(
      &mut rt,
      "a",
      u32::MAX,
      FailurePolicy::retry(2, backoff),
      sink.clone(),
    )
    .await;

    let mailbox = {
      let registry = rt.registry.lock().unwrap();
      registry.get(&actor_id("a")).unwrap().mailbox().clone()
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let delivery = Delivery::new(Message::empty("go"), Ack::Complete(tx));
    assert!(mailbox.send(delivery).await.is_ok());

    let outcome = rx.await.unwrap();
    assert!(outcome.is_ok(), "dead-lettered exhausted retry reports Ok");
    assert_eq!(sink.letters.lock().unwrap().len(), 1);
  }

  #[tokio::test]
  async fn fail_dead_letters_the_triggering_message_and_still_dies() {
    // Under `fail` with a sink, the triggering message is preserved (reason
    // Failed) before the node stops — but the death/ack behavior is unchanged
    // from slice 1/2: teardown runs, Health records a death, the errored outcome
    // is still reported, and the node deregisters + fires the death listener.
    let mut rt = Runtime::new();
    let (dead, notify) = record_deaths(&mut rt);
    let sink = RecorderDeadLetter::new();
    let (health, probe) =
      spawn_failing_with_dead_letter(&mut rt, "a", u32::MAX, FailurePolicy::fail(), sink.clone())
        .await;

    rt.deliver(
      &actor_id("a"),
      Message::json("boom", serde_json::json!({ "x": 9 })),
    )
    .await
    .unwrap();

    // The death signal fires once the failing handle breaks the loop.
    notify.notified().await;

    // The sink received the triggering message (reason Failed).
    {
      let letters = sink.letters.lock().unwrap();
      assert_eq!(letters.len(), 1);
      let letter = &letters[0];
      assert_eq!(letter.msg.type_, "boom");
      assert_eq!(letter.node, actor_id("a"));
      assert_eq!(
        letter.reason,
        DeadLetterReason::Failed {
          error: "handle failed: intentional".to_owned(),
        }
      );
    }

    // Slice 1/2 behavior intact: teardown ran, one handle call, the errored
    // outcome is still reported *and* the death is a distinct event.
    assert!(probe.teardown_called.load(Ordering::SeqCst));
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 1);
    assert_eq!(health.died(), 1);
    assert_eq!(dead.lock().unwrap().as_slice(), &[actor_id("a")]);

    // The node stops resolving.
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn dead_letter_carries_the_triggering_correlation() {
    // The dead letter must carry the *delivery's* correlation so it ties back to
    // the originating run — keyed by correlation id, per the RFC. Drive a
    // delivery with a known correlation and assert the dead letter inherits it.
    let mut rt = Runtime::new();
    let sink = RecorderDeadLetter::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (_health, _probe) = spawn_failing_with_dead_letter(
      &mut rt,
      "a",
      u32::MAX,
      FailurePolicy::retry(1, backoff),
      sink.clone(),
    )
    .await;

    let known = CorrelationId::from("run-dead-letter");
    {
      let (mailbox, health) = {
        let registry = rt.registry.lock().unwrap();
        let handle = registry.get(&actor_id("a")).unwrap();
        (handle.mailbox().clone(), handle.health().clone())
      };
      let delivery =
        Delivery::with_correlation(Message::empty("boom"), Ack::Health(health), known.clone());
      assert!(mailbox.send(delivery).await.is_ok());
    }

    sink.notify.notified().await;

    let letters = sink.letters.lock().unwrap();
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].correlation.as_str(), "run-dead-letter");
  }

  // ---- route_to_error (error output port) -------------------------------------

  use fuchsia_transport::CorrelationId;

  /// An `Emit` sink that records every `(port, message)` emitted on it, plus the
  /// correlation in scope at emit time — so a test can assert the runtime stamped
  /// the error envelope with the triggering delivery's run id.
  struct RecorderEmit {
    emitted: Mutex<Vec<(String, Message)>>,
    correlations: Mutex<Vec<Option<String>>>,
    notify: Notify,
  }

  impl RecorderEmit {
    fn new() -> Arc<Self> {
      Arc::new(Self {
        emitted: Mutex::new(Vec::new()),
        correlations: Mutex::new(Vec::new()),
        notify: Notify::new(),
      })
    }
  }

  impl Emit for RecorderEmit {
    fn emit_to(&self, port: &str, msg: Message) {
      // Capture the correlation in scope when the runtime emits — proving the
      // envelope rides the triggering delivery's run, not a fresh/absent one.
      self
        .correlations
        .lock()
        .unwrap()
        .push(CorrelationId::current().map(|c| c.as_str().to_owned()));
      self.emitted.lock().unwrap().push((port.to_owned(), msg));
      self.notify.notify_one();
    }
  }

  /// Spawn a `FailActor` under `policy` with `recorder` as its `emit` sink.
  async fn spawn_failing_with_emit(
    rt: &mut Runtime,
    id: &str,
    fail_first: u32,
    policy: FailurePolicy,
    recorder: Arc<RecorderEmit>,
  ) -> (Arc<Health>, Arc<FailProbe>) {
    let probe = FailProbe::new(fail_first);
    rt.register(
      "fail",
      FailCreator {
        probe: probe.clone(),
      },
    );
    let config = ActorConfig {
      failure: policy,
      ..Default::default()
    };
    let caps = ActorCapabilities::new().with_emit(recorder);
    let (tx, health, _ports) = rt
      .spawn_with_caps(actor_id(id), "fail", &config, caps)
      .await
      .unwrap();
    drop(tx);
    (health, probe)
  }

  #[tokio::test]
  async fn route_to_error_emits_envelope_and_keeps_going() {
    // Error once, then succeed. The errored message is diverted to the `"error"`
    // port as an envelope; the node keeps handling the next message.
    let mut rt = Runtime::new();
    let recorder = RecorderEmit::new();
    let (health, probe) = spawn_failing_with_emit(
      &mut rt,
      "a",
      1,
      FailurePolicy::route_to_error(),
      recorder.clone(),
    )
    .await;

    // A JSON payload so we can assert the envelope carries the original value.
    let msg = Message::json("reading", serde_json::json!({ "temp": 42 }));
    rt.deliver(&actor_id("a"), msg).await.unwrap();
    recorder.notify.notified().await;

    // Exactly one emission, on the reserved `"error"` port.
    {
      let emitted = recorder.emitted.lock().unwrap();
      assert_eq!(emitted.len(), 1);
      let (port, envelope) = &emitted[0];
      assert_eq!(port, "error");
      assert_eq!(envelope.type_, "error");
      let MessageValue::Json(body) = &envelope.value else {
        panic!("envelope payload should be JSON");
      };
      // The envelope carries the error string, node id, original type + payload.
      assert_eq!(body["error"], "handle failed: intentional");
      assert_eq!(body["node"], "a");
      assert_eq!(body["type"], "reading");
      assert_eq!(body["payload"], serde_json::json!({ "temp": 42 }));
    }

    // Diverted, not retriable: the ack reports `Ok`, so Health counts it handled
    // (not errored) and the node is not dead.
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 0);
    assert_eq!(health.died(), 0);

    // The node kept handling: a second (succeeding) message is processed.
    rt.deliver(&actor_id("a"), Message::empty("two"))
      .await
      .unwrap();
    while probe.calls.load(Ordering::SeqCst) < 2 {
      probe.notify.notified().await;
    }
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    // No second emission — the success path emits nothing on `"error"`.
    assert_eq!(recorder.emitted.lock().unwrap().len(), 1);
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn route_to_error_envelope_carries_the_triggering_correlation() {
    // The error envelope must be stamped with the *delivery's* correlation so the
    // right run's error branch fires — even though it's emitted after `handle`'s
    // own scope has ended.
    let mut rt = Runtime::new();
    let recorder = RecorderEmit::new();
    let (_health, _probe) = spawn_failing_with_emit(
      &mut rt,
      "a",
      u32::MAX, // always error
      FailurePolicy::route_to_error(),
      recorder.clone(),
    )
    .await;

    // Push through the engine-style path with a known correlation, so we can
    // assert the envelope inherits exactly it. `deliver` mints a fresh id per
    // delivery; to control it we send a delivery carrying our own correlation.
    let known = CorrelationId::from("run-route-to-error");
    {
      let (mailbox, health) = {
        let registry = rt.registry.lock().unwrap();
        let handle = registry.get(&actor_id("a")).unwrap();
        (handle.mailbox().clone(), handle.health().clone())
      };
      let delivery =
        Delivery::with_correlation(Message::empty("boom"), Ack::Health(health), known.clone());
      assert!(mailbox.send(delivery).await.is_ok());
    }

    recorder.notify.notified().await;

    let correlations = recorder.correlations.lock().unwrap();
    assert_eq!(correlations.len(), 1);
    assert_eq!(correlations[0].as_deref(), Some("run-route-to-error"));
  }

  #[tokio::test]
  async fn route_to_error_encodes_binary_payload_as_byte_length_marker() {
    // A binary payload can't be embedded in the JSON envelope, so it's rendered
    // as a `{ "byte_len": <len> }` marker; `bytes` is reserved for a base64 of
    // the content if binary logging is added later.
    let mut rt = Runtime::new();
    let recorder = RecorderEmit::new();
    let _ = spawn_failing_with_emit(
      &mut rt,
      "a",
      1,
      FailurePolicy::route_to_error(),
      recorder.clone(),
    )
    .await;

    rt.deliver(&actor_id("a"), Message::binary("blob", vec![1, 2, 3, 4, 5]))
      .await
      .unwrap();
    recorder.notify.notified().await;

    let emitted = recorder.emitted.lock().unwrap();
    let MessageValue::Json(body) = &emitted[0].1.value else {
      panic!("envelope payload should be JSON");
    };
    assert_eq!(body["payload"], serde_json::json!({ "byte_len": 5 }));
  }

  // ---- restart (slice 5) ------------------------------------------------------

  /// Shared observation for a restart-supervised node: how many incarnations
  /// have been `setup` (each rebuild bumps it), every message a *surviving*
  /// incarnation handled, and how many of the first handle calls should panic.
  struct RestartProbe {
    /// `handle` calls 1..=`panic_first` panic; after that they record + succeed.
    panic_first: u32,
    handle_calls: AtomicU64,
    setups: AtomicU64,
    handled: Mutex<Vec<String>>,
    notify: Notify,
  }

  impl RestartProbe {
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

  /// An actor that panics on its first `panic_first` handle calls (counted across
  /// incarnations on the shared probe), then records + succeeds. Each rebuild
  /// runs `setup`, bumping the probe's incarnation count — so a test can observe
  /// that a fresh `&mut self` ran `setup` again.
  struct RestartActor {
    probe: Arc<RestartProbe>,
  }

  #[async_trait]
  impl Actor for RestartActor {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      self.probe.setups.fetch_add(1, Ordering::SeqCst);
      Ok(())
    }
    async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
      let n = self.probe.handle_calls.fetch_add(1, Ordering::SeqCst) + 1;
      if (n as u32) <= self.probe.panic_first {
        // Panic mid-handle: the restart supervisor catches this, discards the
        // actor, and rebuilds — the in-flight message is dropped, not re-fed.
        panic!("intentional panic in handle (call {n})");
      }
      self.probe.handled.lock().unwrap().push(msg.type_.clone());
      self.probe.notify.notify_one();
      Ok(())
    }
  }

  struct RestartCreator {
    probe: Arc<RestartProbe>,
  }

  impl ActorCreator for RestartCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(RestartActor {
        probe: self.probe.clone(),
      }))
    }
  }

  /// Spawn a `RestartActor` under a restart policy, returning its mailbox, its
  /// health, and the shared probe. Keeps the caller's `tx` (returned) so a test
  /// controls when the mailbox closes; the registry holds its own.
  async fn spawn_restartable(
    rt: &mut Runtime,
    id: &str,
    panic_first: u32,
    policy: FailurePolicy,
    sink: Option<Arc<RecorderDeadLetter>>,
  ) -> (MailboxTx, Arc<Health>, Arc<RestartProbe>) {
    let probe = RestartProbe::new(panic_first);
    rt.register(
      "restart",
      RestartCreator {
        probe: probe.clone(),
      },
    );
    let config = ActorConfig {
      failure: policy,
      ..Default::default()
    };
    let mut caps = ActorCapabilities::new();
    if let Some(sink) = sink {
      caps.insert::<dyn DeadLetter>(sink);
    }
    let (tx, health, _ports) = rt
      .spawn_with_caps(actor_id(id), "restart", &config, caps)
      .await
      .unwrap();
    (tx, health, probe)
  }

  #[tokio::test]
  async fn restart_rebuilds_and_a_queued_message_is_processed_by_the_new_incarnation() {
    // A node that panics on its first handle, with budget. We queue *two*
    // messages up front: the first panics (caught → rebuild), the second is
    // drained by the fresh incarnation. The rebuild is observable (a second
    // `setup`), the queue survived the crash, and the node stays registered.
    let mut rt = Runtime::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (tx, health, probe) = spawn_restartable(
      &mut rt,
      "a",
      1, // first handle panics, then recover
      FailurePolicy::restart(3, backoff),
      None,
    )
    .await;

    // Queue both messages before the first is even handled, so the second is
    // genuinely waiting in the *same* mailbox across the crash.
    rt.deliver(&actor_id("a"), Message::empty("boom"))
      .await
      .unwrap();
    rt.deliver(&actor_id("a"), Message::empty("survivor"))
      .await
      .unwrap();

    // Wait until the surviving incarnation handles the second message.
    loop {
      if !probe.handled.lock().unwrap().is_empty() {
        break;
      }
      probe.notify.notified().await;
    }

    // The fresh incarnation handled the queued "survivor"; the panicking "boom"
    // was dropped, not re-fed (so it does not appear, and didn't loop).
    assert_eq!(probe.handled.lock().unwrap().as_slice(), &["survivor"]);
    // Two incarnations were set up: the initial spawn + one rebuild after the
    // crash — proving a fresh `&mut self` with `setup` re-run.
    assert_eq!(probe.setups.load(Ordering::SeqCst), 2);
    // The panic was *not* recorded as a per-message error (the ack dropped); the
    // recovered message was handled.
    assert_eq!(health.handled(), 1);
    // The node never deregistered — a transient restart keeps it resolving.
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
    drop(tx);
  }

  #[tokio::test]
  async fn restart_budget_exhausted_dies_and_drains_queue_to_dead_letter() {
    // Always panics; max_restarts=2 → 3 incarnations, each crashing on one
    // message it pulls (a restart is triggered by a *crash*, which needs a
    // message to crash on). So three crash-messages exhaust the budget; the
    // fourth — a bystander queued behind them — the dead node never handles and
    // is drained to the dead-letter sink with reason NodeDied.
    let mut rt = Runtime::new();
    let (dead, notify) = record_deaths(&mut rt);
    let sink = RecorderDeadLetter::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    let (tx, health, probe) = spawn_restartable(
      &mut rt,
      "a",
      u32::MAX, // always panic
      FailurePolicy::restart(2, backoff),
      Some(sink.clone()),
    )
    .await;

    // Three crash-messages (one per incarnation) + a bystander behind them.
    for label in ["crash-1", "crash-2", "crash-3"] {
      rt.deliver(&actor_id("a"), Message::empty(label))
        .await
        .unwrap();
    }
    rt.deliver(&actor_id("a"), Message::empty("bystander"))
      .await
      .unwrap();

    // The death signal fires once the budget is exhausted.
    notify.notified().await;

    // 3 incarnations: initial + 2 restarts, each crashing on one message; nothing
    // was handled to completion.
    assert_eq!(probe.setups.load(Ordering::SeqCst), 3);
    assert_eq!(health.handled(), 0);
    assert_eq!(health.died(), 1);
    assert_eq!(dead.lock().unwrap().as_slice(), &[actor_id("a")]);

    // Deregistered — stops resolving.
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());

    // The bystander was drained to the dead-letter sink (reason NodeDied,
    // restarts=2). The crash messages' acks just dropped (not letters).
    let letters = sink.letters.lock().unwrap();
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].msg.type_, "bystander");
    assert_eq!(letters[0].node, actor_id("a"));
    assert_eq!(
      letters[0].reason,
      DeadLetterReason::NodeDied { restarts: 2 }
    );
    drop(tx);
  }

  #[tokio::test]
  async fn default_node_panic_is_a_death_with_no_restart() {
    // Regression guard for the perf-critical default path: max_restarts=0 (the
    // default) keeps slice 1's behavior exactly — a panic is a death, the node is
    // never rebuilt, and it deregisters. Identical to
    // `panicking_handle_is_detected_as_a_death`, but via the restart-aware
    // `commit` to prove the default branch is unchanged.
    let mut rt = Runtime::new();
    let (dead, notify) = record_deaths(&mut rt);
    // FailurePolicy::default() = max_restarts 0.
    let (tx, health, probe) =
      spawn_restartable(&mut rt, "a", u32::MAX, FailurePolicy::default(), None).await;
    drop(tx);

    rt.deliver(&actor_id("a"), Message::empty("boom"))
      .await
      .unwrap();
    notify.notified().await;

    // Exactly one incarnation — no rebuild.
    assert_eq!(probe.setups.load(Ordering::SeqCst), 1);
    assert_eq!(health.died(), 1);
    assert_eq!(dead.lock().unwrap().as_slice(), &[actor_id("a")]);
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn fail_policy_is_not_restarted_even_with_budget() {
    // A `fail`-policy stop is a *deliberate* shutdown, so it is never restarted
    // even with restart budget available. Build a policy with both `fail` and a
    // restart budget; an errored handle stops the node permanently.
    let mut rt = Runtime::new();
    let (dead, notify) = record_deaths(&mut rt);
    let probe = FailProbe::new(u32::MAX); // always errors (returns Err, not panic)
    rt.register(
      "fail",
      FailCreator {
        probe: probe.clone(),
      },
    );
    // Both fail and a restart budget on the same policy. Both `FailurePolicy` and
    // `RestartPolicy` are `#[non_exhaustive]`, so build from a constructor then
    // mutate the public field rather than a struct literal.
    let mut policy = FailurePolicy::restart(5, Backoff::fixed(Duration::from_millis(1)));
    policy.on_error = OnError::Fail;
    let config = ActorConfig {
      failure: policy,
      ..Default::default()
    };
    let (tx, health, _ports) = rt
      .spawn_with_caps(actor_id("a"), "fail", &config, ActorCapabilities::new())
      .await
      .unwrap();
    drop(tx);

    rt.deliver(&actor_id("a"), Message::empty("boom"))
      .await
      .unwrap();
    notify.notified().await;

    // Exactly one handle call — the `fail` stop was *not* restarted despite the
    // budget. The node died once and stays dead.
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(health.died(), 1);
    assert_eq!(dead.lock().unwrap().as_slice(), &[actor_id("a")]);
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn restart_waits_the_backoff_between_rebuilds() {
    // Loose, non-flaky lower bound: always-panic with max_restarts=2 and a 30ms
    // fixed backoff. Each incarnation crashes on one queued message, so three
    // crash-messages exhaust the budget; the two backoffs (between incarnations
    // 1→2 and 2→3) must add up to at least ~2×30ms before the death fires.
    let mut rt = Runtime::new();
    let (_dead, notify) = record_deaths(&mut rt);
    let backoff = Backoff::fixed(Duration::from_millis(30));
    let (tx, _health, _probe) = spawn_restartable(
      &mut rt,
      "a",
      u32::MAX,
      FailurePolicy::restart(2, backoff),
      None,
    )
    .await;
    drop(tx);

    let start = Instant::now();
    for label in ["crash-1", "crash-2", "crash-3"] {
      rt.deliver(&actor_id("a"), Message::empty(label))
        .await
        .unwrap();
    }
    notify.notified().await;
    let elapsed = start.elapsed();

    // 2 restarts → 2 backoffs of 30ms between the three incarnations → at least
    // ~50ms even with scheduling slack. Lower bound only, so it stays non-flaky.
    assert!(
      elapsed >= Duration::from_millis(50),
      "expected restart backoff, took {elapsed:?}"
    );
  }

  // ---- poison-message quarantine (slice 6) ------------------------------------

  /// Send a delivery with an explicit `attempts` count straight into a node's
  /// mailbox (a `Health` ack), bypassing `deliver` (which always stamps `1`) so a
  /// test can drive the cross-delivery counter the poison gate reads.
  async fn deliver_attempt(rt: &Runtime, id: &str, type_: &str, attempts: u32) {
    let (mailbox, health) = {
      let registry = rt.registry.lock().unwrap();
      let handle = registry.get(&actor_id(id)).unwrap();
      (handle.mailbox().clone(), handle.health().clone())
    };
    let delivery =
      Delivery::new(Message::empty(type_), Ack::Health(health)).with_attempts(attempts);
    assert!(mailbox.send(delivery).await.is_ok());
  }

  #[tokio::test]
  async fn poison_delivery_is_quarantined_to_the_sink_without_handling() {
    // A delivery whose attempts exceed `poison_after` is diverted to the
    // dead-letter sink (reason Poison) *without* `handle` being called, the node
    // survives to handle the next (in-budget) message.
    let mut rt = Runtime::new();
    let sink = RecorderDeadLetter::new();
    // `poison_after: 2`, otherwise default (continue, no restart). The actor would
    // succeed on every call, so a handle of the poison message would NOT error —
    // proving the divert is the *gate*, not the failure policy.
    let policy = FailurePolicy::poison_after(2);
    let (health, probe) =
      spawn_failing_with_dead_letter(&mut rt, "a", 0, policy, sink.clone()).await;

    // attempts=3 > poison_after=2 → quarantined before handle.
    deliver_attempt(&rt, "a", "poison", 3).await;
    sink.notify.notified().await;

    // The sink got the poison message with reason Poison { attempts: 3 }.
    {
      let letters = sink.letters.lock().unwrap();
      assert_eq!(letters.len(), 1);
      assert_eq!(letters[0].msg.type_, "poison");
      assert_eq!(letters[0].node, actor_id("a"));
      assert_eq!(letters[0].reason, DeadLetterReason::Poison { attempts: 3 });
    }
    // `handle` was NOT called for the poison message.
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    // The quarantine reports `Ok` on the ack (so a feeder stops re-delivering),
    // which folds into `handled` for a Health ack — matching every other
    // survive-and-quarantine path (route-to-error, dead-lettered retry). It is
    // *not* an error, and the sink absorbed the poison so `poisoned` stays 0.
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 0);
    assert_eq!(health.poisoned(), 0);

    // The node survives: a normal (attempts=1) message is handled.
    rt.deliver(&actor_id("a"), Message::empty("ok"))
      .await
      .unwrap();
    while probe.calls.load(Ordering::SeqCst) < 1 {
      probe.notify.notified().await;
    }
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn poison_quarantine_acks_ok_through_a_durable_ack() {
    // The quarantine reports `Ok` on the ack so an at-least-once feeder stops
    // re-delivering. Drive a `Complete` (durable) ack and assert the outcome is
    // Ok even though the message was never handled.
    let mut rt = Runtime::new();
    let sink = RecorderDeadLetter::new();
    let (_health, probe) = spawn_failing_with_dead_letter(
      &mut rt,
      "a",
      u32::MAX, // would always error *if* handled — but it must not be handled
      FailurePolicy::poison_after(2),
      sink.clone(),
    )
    .await;

    let mailbox = {
      let registry = rt.registry.lock().unwrap();
      registry.get(&actor_id("a")).unwrap().mailbox().clone()
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let delivery = Delivery::new(Message::empty("poison"), Ack::Complete(tx)).with_attempts(5);
    assert!(mailbox.send(delivery).await.is_ok());

    let outcome = rx.await.unwrap();
    assert!(
      outcome.is_ok(),
      "quarantined poison reports Ok to the feeder"
    );
    // Never handled (so the always-error actor never even ran).
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(sink.letters.lock().unwrap().len(), 1);
  }

  #[tokio::test]
  async fn poison_after_zero_is_disabled_high_attempts_handled_normally() {
    // Regression guard: `poison_after: 0` (the default) disables quarantine, so a
    // high-attempts delivery is handled normally — exactly slice-5 behavior.
    let mut rt = Runtime::new();
    let sink = RecorderDeadLetter::new();
    // Default policy = poison_after 0; actor succeeds.
    let (health, probe) =
      spawn_failing_with_dead_letter(&mut rt, "a", 0, FailurePolicy::default(), sink.clone()).await;

    // A wildly high attempts count must NOT be quarantined when disabled.
    deliver_attempt(&rt, "a", "high", 999).await;
    while probe.calls.load(Ordering::SeqCst) < 1 {
      probe.notify.notified().await;
    }

    // Handled normally; nothing quarantined.
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(health.handled(), 1);
    assert_eq!(health.poisoned(), 0);
    assert_eq!(sink.letters.lock().unwrap().len(), 0);
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn poison_quarantine_no_sink_counts_on_health_and_drops() {
    // No dead-letter sink: a poison delivery falls back to the Health poisoned
    // counter + drop, and the node survives.
    let mut rt = Runtime::new();
    // poison_after 2, no sink granted.
    let (health, probe) = spawn_failing(&mut rt, "a", 0, FailurePolicy::poison_after(2)).await;

    deliver_attempt(&rt, "a", "poison", 3).await;
    // Wait for the poisoned counter to tick (the divert is synchronous on the
    // recv loop, so a short spin suffices).
    while health.poisoned() == 0 {
      tokio::time::sleep(Duration::from_millis(2)).await;
    }

    // Counted as a distinct poisoned outcome, dropped, never handled. The Ok
    // ack folds into `handled` (as on every quarantine path); the distinct
    // `poisoned` counter is what marks it as a poison drop, and `errored`/`died`
    // stay clear.
    assert_eq!(health.poisoned(), 1);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 0);
    assert_eq!(health.died(), 0);

    // The node survives for the next (in-budget) message.
    rt.deliver(&actor_id("a"), Message::empty("ok"))
      .await
      .unwrap();
    while probe.calls.load(Ordering::SeqCst) < 1 {
      probe.notify.notified().await;
    }
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
  }

  #[tokio::test]
  async fn poison_message_redeliveries_spare_the_restart_budget_and_quarantine() {
    // **Mechanism B + A together.** A restart-enabled node that *always* panics,
    // fed the *same* message as re-deliveries (attempts 1,2,3,4) with a small
    // restart budget and `poison_after: 3`:
    //   - attempts=1 panics → charges 1 restart (first attempt → node-attributed).
    //   - attempts=2,3 panic → rebuild WITHOUT charging (re-delivery →
    //     message-attributed): the budget is spared.
    //   - attempts=4 > poison_after=3 → quarantined (mechanism A): diverted to the
    //     sink, Ok-acked, node spared.
    // So the node survives `max_restarts=1` despite four panic-inducing
    // deliveries, and never dies.
    let mut rt = Runtime::new();
    let (dead, _notify) = record_deaths(&mut rt);
    let sink = RecorderDeadLetter::new();
    let backoff = Backoff::fixed(Duration::from_millis(1));
    // max_restarts=1, poison_after=3 — quarantine fires before the (1) budget can
    // be re-charged by repeated first attempts.
    let mut policy = FailurePolicy::restart(1, backoff);
    policy.poison_after = 3;
    let (tx, health, probe) =
      spawn_restartable(&mut rt, "a", u32::MAX, policy, Some(sink.clone())).await;

    // Feed the same message four times, simulating a feeder incrementing attempts
    // per re-delivery. We send straight into the mailbox with explicit attempts.
    let mailbox = {
      let registry = rt.registry.lock().unwrap();
      registry.get(&actor_id("a")).unwrap().mailbox().clone()
    };
    for attempt in 1..=4u32 {
      let (ack_tx, _ack_rx) = tokio::sync::oneshot::channel();
      let delivery =
        Delivery::new(Message::empty("poison"), Ack::Complete(ack_tx)).with_attempts(attempt);
      assert!(mailbox.send(delivery).await.is_ok());
    }

    // Wait until the 4th delivery is quarantined to the sink.
    sink.notify.notified().await;

    // The poison was quarantined at attempts=4 (> poison_after=3), node spared.
    {
      let letters = sink.letters.lock().unwrap();
      assert_eq!(letters.len(), 1);
      assert_eq!(letters[0].reason, DeadLetterReason::Poison { attempts: 4 });
    }
    // The node never died: the re-delivery crashes (attempts 2,3) did NOT charge
    // the (max_restarts=1) budget, and the first attempt's single restart left
    // budget intact. So it's still registered and never recorded a death.
    assert_eq!(health.died(), 0);
    assert!(dead.lock().unwrap().is_empty());
    assert!(rt.registry_contains(&actor_id("a")).unwrap());
    // It did rebuild (the panics): setups = initial + at least the first-attempt
    // restart. We don't pin the exact count (re-delivery rebuilds also bump it),
    // only that it stayed alive.
    assert!(probe.setups.load(Ordering::SeqCst) >= 2);
    drop(tx);
  }

  #[tokio::test]
  async fn distinct_first_attempt_crashes_burn_the_budget_and_die() {
    // The contrast to the poison case: a node that panics on *distinct*
    // first-attempt (attempts=1) messages charges the budget on each crash and
    // dies once it's exhausted — proving mechanism B spares only *re-deliveries*,
    // not first attempts (a genuinely sick node still dies).
    let mut rt = Runtime::new();
    let (dead, notify) = record_deaths(&mut rt);
    let backoff = Backoff::fixed(Duration::from_millis(1));
    // max_restarts=2, poison_after=5 (high, so it never fires here — all attempts
    // are 1). Three first-attempt crashes exhaust the (2) budget.
    let mut policy = FailurePolicy::restart(2, backoff);
    policy.poison_after = 5;
    let (tx, health, probe) = spawn_restartable(&mut rt, "a", u32::MAX, policy, None).await;
    drop(tx);

    // Three distinct first-attempt (attempts=1, via `deliver`) crash messages.
    for label in ["m1", "m2", "m3"] {
      rt.deliver(&actor_id("a"), Message::empty(label))
        .await
        .unwrap();
    }

    // The death fires once the budget is exhausted by the three first-attempt
    // crashes (initial + 2 restarts).
    notify.notified().await;

    assert_eq!(health.died(), 1);
    assert_eq!(dead.lock().unwrap().as_slice(), &[actor_id("a")]);
    assert!(!rt.registry_contains(&actor_id("a")).unwrap());
    // 3 incarnations were set up: initial + 2 charged restarts.
    assert_eq!(probe.setups.load(Ordering::SeqCst), 3);
  }
}
