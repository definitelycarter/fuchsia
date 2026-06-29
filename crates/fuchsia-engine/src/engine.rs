use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use fuchsia_actor::{
  ActorCapabilities, ActorConfig, ActorCreator, ActorId, Emit, Message, OutputPorts,
};
use fuchsia_runtime::{RestartControl, Runtime, RuntimeError};
use fuchsia_transport::{Ack, CorrelationId, Delivery, Health, MailboxTx, Outcome};
use tokio::sync::{Mutex, oneshot};

use crate::error::EngineError;
use crate::router::{RouteCounts, RoutedEmit, RouterState};

/// What the engine retains for a restart-enabled node so it can drive
/// `restart_node` — in particular, **revive** a permanently-dead node.
///
/// The retention of these bits is the single load-bearing decision of the
/// restart-revival design: when a node's budget is exhausted, the runtime's
/// death listener deregisters it from the router (dropping the router's copy of
/// its mailbox/health/ports), but the supervisor task does **not** exit — it
/// parks holding `rx` + the rebuild recipe. So the *recipe* lives on in the
/// runtime; what the **engine** must separately keep is everything needed to put
/// the node *back into the router* on revival (the router can't reconstruct a
/// mailbox), plus the control handle to wake the parked supervisor. Keeping them
/// here — rather than re-deriving them — is what makes "revive a dead node" a
/// pure restore + signal, with no rebuild path duplicated in the engine.
struct RestartHandle {
  control: RestartControl,
  mailbox: MailboxTx,
  health: Arc<Health>,
  output_ports: OutputPorts,
}

/// Routes messages between actors according to a graph's edges.
///
/// All methods take `&self` so the engine can be shared as `Arc<Engine>` —
/// the host, ingress, and scheduler each hold a clone and use it
/// concurrently. The router (the hot path: `push` and routing) is a plain
/// `RwLock` with many readers; only the cold mutating paths (`register`,
/// `add_node`, `remove_graph`) lock the runtime.
///
/// The engine knows only actors and addressing — not how the graph was
/// authored. A layer above it (host code, or a downstream config loader)
/// translates a higher-level definition into the nodes and edges fed here.
pub struct Engine {
  runtime: Mutex<Runtime>,
  router: Arc<RwLock<RouterState>>,
  // Restart handles for restart-enabled nodes, keyed by id — what `restart_node`
  // needs to revive a dead node (restore its router entry) or force a live one.
  // A separate `Mutex` (not under the runtime lock) since it's touched only on
  // the cold `add_node` / `restart_node` paths, never per message.
  restart_handles: Mutex<HashMap<ActorId, RestartHandle>>,
}

impl Engine {
  pub fn new() -> Self {
    let router = Arc::new(RwLock::new(RouterState::default()));

    // Install the runtime's death seam: when a node's task dies (a panic or an
    // abnormal exit), the runtime supervisor calls this with the dead id, and
    // the engine drops it from its router so it stops resolving as a routable
    // target — an upstream emit to it then reads as `no_route`/shed, not a
    // silent offer into a permanently dead mailbox. A refcount bump of the
    // router handle so the listener can address the live table from a
    // supervisor task.
    let mut runtime = Runtime::new();
    let router_for_death = Arc::clone(&router);
    runtime.on_death(Arc::new(move |id: &ActorId| {
      // A poisoned router lock means a prior panic mid-mutation; the node is
      // already unreachable in practice, so dropping here is best-effort.
      if let Ok(mut state) = router_for_death.write() {
        state.deregister(id);
      }
    }));

    Self {
      runtime: Mutex::new(runtime),
      router,
      restart_handles: Mutex::new(HashMap::new()),
    }
  }

  /// Register an actor type so nodes can be instantiated from it.
  pub async fn register(&self, type_name: impl Into<String>, creator: impl ActorCreator) {
    self.runtime.lock().await.register(type_name, creator);
  }

  /// Instantiate a node as an actor and register it as a routable target. The
  /// node's group lives in its `ActorId`.
  ///
  /// `caps` carries the capabilities the *caller* grants this node (e.g. a
  /// host-defined state sink) — empty for a plain routing node. The
  /// engine adds the one capability it owns, `emit` (routing through this
  /// engine); the runtime adds `schedule`. The engine never inspects `caps`, so
  /// it stays binding-agnostic.
  pub async fn add_node(
    &self,
    id: ActorId,
    type_name: &str,
    config: &ActorConfig,
    caps: ActorCapabilities,
  ) -> Result<(), EngineError> {
    let emit = Arc::new(RoutedEmit {
      source: id.clone(),
      router: self.router.clone(),
    });
    let caps = caps.with_emit(emit);

    // Prepare under the runtime lock, then run `setup` *without* the lock so a
    // slow async setup (one that does I/O) can't serialize every other graph
    // mutation behind the runtime mutex; finally commit under the lock.
    let mut spawning = {
      let mut runtime = self.runtime.lock().await;
      runtime.prepare(id.clone(), type_name, config, caps)?
    };
    spawning.setup().await?;
    // `commit` hands back the node's declared output ports (from its resolved
    // creator) alongside the mailbox/health, plus — for a restart-enabled node —
    // a `RestartControl` the engine retains to drive `restart_node`.
    let committed = {
      let mut runtime = self.runtime.lock().await;
      runtime.commit(spawning)?
    };

    self
      .router
      .write()
      .map_err(|_| EngineError::Lock)?
      .register(
        // Refcount bumps so the engine can keep its own routable copy of the
        // node's mailbox/health for revival (see `restart_node`) while the router
        // holds the live one. Cold per-`add_node` path.
        id.clone(),
        committed.mailbox.clone(),
        committed.health.clone(),
        committed.output_ports.clone(),
      );

    // Retain a *restart handle* for a restart-enabled node: the control + the
    // bits needed to re-register a dead node's router entry on revival (the
    // death listener removes it on permanent death, so the engine must keep its
    // own copy to restore it). A default node gets no handle and cannot be
    // restarted/revived. Cold path, only when the node opted in.
    if let Some(control) = committed.restart {
      self.restart_handles.lock().await.insert(
        id,
        RestartHandle {
          control,
          mailbox: committed.mailbox,
          health: committed.health,
          output_ports: committed.output_ports,
        },
      );
    }
    Ok(())
  }

  /// Force or revive a node's restart, the public face of the runtime's restart
  /// supervisor (only a node spawned with `failure.restart.max_restarts > 0` can
  /// be restarted; a default node returns [`EngineError::NotFound`]).
  ///
  /// - A node the router shows as **dead** (its budget was exhausted and it
  ///   deregistered) is **revived**: its router entry is restored from the
  ///   engine's retained restart handle so it resolves again, its supervisor's
  ///   budget is reset, and it resumes draining its surviving mailbox.
  /// - A **live** node with `force` is torn down (`teardown`) and rebuilt with
  ///   fresh state, the mailbox surviving; its budget is reset.
  /// - A **live** node **without** `force` is rejected as already-running
  ///   ([`RuntimeError::AlreadyRunning`] via [`EngineError::Runtime`]).
  ///
  /// Either way a manual restart **resets** the automatic backoff/limit budget —
  /// an operator's deliberate "try again," distinct from the automatic budget.
  pub async fn restart_node(&self, id: &ActorId, force: bool) -> Result<(), EngineError> {
    let handles = self.restart_handles.lock().await;
    // No restart handle → either the node never existed or it is a default
    // (restart-disabled) node, which cannot be restarted. Surfaced as NotFound.
    let handle = handles
      .get(id)
      .ok_or_else(|| EngineError::NotFound(id.clone()))?;

    if handle.control.is_dead() {
      // Dead (parked) node → revive. Restore the router entry *first* (so it
      // resolves the instant the supervisor resumes), then signal the revive.
      // Reviving never needs `force`; a dead node always revives.
      self
        .router
        .write()
        .map_err(|_| EngineError::Lock)?
        .register(
          id.clone(),
          // Refcount bumps of the retained mailbox/health/ports the engine kept
          // precisely so a dead node's router entry can be restored.
          handle.mailbox.clone(),
          handle.health.clone(),
          handle.output_ports.clone(),
        );
      handle.control.request_restart(false);
      return Ok(());
    }

    // Live node: only a forced restart is allowed; otherwise it's already
    // running. The router entry stays put (the mailbox survives), so nothing to
    // re-register — just signal the supervisor to teardown + rebuild.
    if !force {
      return Err(EngineError::Runtime(RuntimeError::AlreadyRunning(
        id.clone(),
      )));
    }
    handle.control.request_restart(true);
    Ok(())
  }

  /// Add a directed edge from `from`'s named output `port` to `to`'s mailbox.
  /// Only emissions `from` makes *on that port* flow to `to`; a port may still
  /// have several edges, so fan-out within a port is preserved.
  ///
  /// Rejects an edge whose `port` a [`Fixed`](fuchsia_actor::OutputPorts::Fixed)
  /// source node does not declare ([`EngineError::UnknownPort`]); `"out"` is
  /// always allowed and `"error"` is reserved. A
  /// [`Dynamic`](fuchsia_actor::OutputPorts::Dynamic) source accepts any port.
  ///
  /// Also rejects an edge that would close a cycle — a self-loop, or an edge
  /// whose target already reaches its source over the existing edges
  /// ([`EngineError::Cycle`]) — leaving the graph unchanged, so a running graph
  /// is always acyclic.
  pub fn add_edge(&self, from: ActorId, port: &str, to: ActorId) -> Result<(), EngineError> {
    self
      .router
      .write()
      .map_err(|_| EngineError::Lock)?
      .add_edge(from, port, to)
  }

  /// Add an edge from `from`'s default `"out"` port to `to` — the terse,
  /// two-node form for the common single-output wiring. Equivalent to
  /// `add_edge(from, "out", to)`.
  pub fn add_default_edge(&self, from: ActorId, to: ActorId) -> Result<(), EngineError> {
    self.add_edge(from, fuchsia_actor::DEFAULT_PORT, to)
  }

  /// Read the route-outcome counters for one `(node, port)` —
  /// `delivered` / `shed` / `no_route`, zeroed if nothing has routed there yet.
  /// In-process observability.
  ///
  /// Counters for a node's *declared* ports (a `Fixed` node's ports, plus the
  /// always-emittable `"out"`/`"error"`) and any *wired* port are tracked
  /// per-port. An emit on a port that was **neither declared nor wired** (only
  /// possible on a `Dynamic` node) is still counted, but on the node's
  /// per-node fallback — see [`route_counts_fallback`](Self::route_counts_fallback)
  /// — so it reads as zero here. Nothing routes silently.
  pub fn route_counts(&self, node: &ActorId, port: &str) -> Result<RouteCounts, EngineError> {
    Ok(
      self
        .router
        .read()
        .map_err(|_| EngineError::Lock)?
        .route_counts(node, port),
    )
  }

  /// Read a node's per-node **fallback** route counters — the bucket for
  /// emissions on a port that was neither declared nor wired. Zeroed for an
  /// unknown node. In-process observability.
  pub fn route_counts_fallback(&self, node: &ActorId) -> Result<RouteCounts, EngineError> {
    Ok(
      self
        .router
        .read()
        .map_err(|_| EngineError::Lock)?
        .route_counts_fallback(node),
    )
  }

  /// The `emit` sink a node `source` was given — the same [`RoutedEmit`]
  /// injected at `add_node`, addressing the live routing table. Calling
  /// `.emit_to(port, msg)` on it runs exactly the routing hot path (`route` +
  /// the per-port counter bump) *without* the actor task or the recv loop.
  ///
  /// Exposed only as a **benchmarking seam** — `#[doc(hidden)]`, not part of the
  /// supported surface — so the routing path can be measured in isolation. The
  /// node need not exist as a target; the sink simply routes from `source`'s id.
  #[doc(hidden)]
  pub fn emit_sink(&self, source: ActorId) -> Arc<dyn Emit> {
    // Refcount bump on the shared router handle so the sink can outlive this
    // call, matching how `add_node` builds a node's own `emit`.
    Arc::new(RoutedEmit {
      source,
      router: Arc::clone(&self.router),
    })
  }

  /// Tear down a whole graph: stop every actor in `group` and drop its edges.
  /// Scoped to the group — other graphs are untouched. Cross-group edges into
  /// this group simply stop resolving (a graceful drop), so the assembler is
  /// free to remove a graph whether or not others still reference it.
  pub async fn remove_graph(&self, group: &str) -> Result<(), EngineError> {
    let ids = self
      .router
      .read()
      .map_err(|_| EngineError::Lock)?
      .ids_in_group(group);

    {
      let mut runtime = self.runtime.lock().await;
      for id in &ids {
        runtime.stop(id)?;
      }
    }

    // Drop the group's restart handles. For a restart-enabled node the engine
    // holds the only remaining *strong* mailbox sender (the supervisor holds a
    // weak one); dropping it here lets `rx` close after `runtime.stop` dropped
    // the registry's sender, so the supervisor reaches a clean shutdown instead
    // of staying alive on the engine's retained sender. Also stops a removed
    // node from being revivable. `ids_in_group` covers live nodes; sweep the map
    // by group to also clear any *parked-dead* node (no longer a router target).
    self
      .restart_handles
      .lock()
      .await
      .retain(|id, _| id.group() != group);

    self
      .router
      .write()
      .map_err(|_| EngineError::Lock)?
      .remove_group(group);
    Ok(())
  }

  /// Deliver an external event into an entrypoint actor's mailbox.
  ///
  /// Called by the host's ingress layer, whose demux maps a key (a webhook
  /// endpoint, an MQTT topic) to an `ActorId`. It's a targeted, id-keyed offer
  /// into one mailbox — not a central pump; internal emissions route through
  /// each actor's `emit`. The ingress *actor* itself receives via its mailbox
  /// and emits onward; it does not call this. Best-effort, at-most-once: the
  /// target's `Health` records the outcome, and an unknown id is `NotFound`.
  ///
  /// `id` is the run's [`CorrelationId`], minted **here at the trigger** and
  /// propagated automatically from this entry through every emit/hop and the
  /// guest boundary (actors and guests never manage it). Mint a fresh one with
  /// [`CorrelationId::new`] when there's nothing to correlate to, or pass an
  /// existing id (an external request/trace id, or a parent run's id). Taking
  /// the id rather than minting-and-returning lets a trigger register its result
  /// collector *before* the run starts, so a fast run can't finish first.
  pub fn push(
    &self,
    entrypoint: &ActorId,
    msg: Message,
    id: CorrelationId,
  ) -> Result<(), EngineError> {
    let state = self.router.read().map_err(|_| EngineError::Lock)?;
    let (mailbox, health) = state
      .target(entrypoint)
      .ok_or_else(|| EngineError::NotFound(entrypoint.clone()))?;
    let _ = mailbox.offer(Delivery::with_correlation(
      msg,
      Ack::Health(health.clone()),
      id,
    ));
    Ok(())
  }

  /// Deliver an external event into an entrypoint's mailbox and **await the
  /// outcome of handling it** — the at-least-once ingress.
  ///
  /// Where [`Engine::push`] is fire-and-forget (at-most-once: a full mailbox
  /// sheds, and its `Ok` means only "offered"), `push_durable` is for a durable
  /// caller — e.g. a worker that has claimed a queued job under a lease and may
  /// delete that job *only once the entrypoint has actually handled the
  /// message*. It delivers with backpressure (the blocking [`MailboxTx::send`],
  /// which waits for room rather than shedding) and carries an [`Ack::Complete`]
  /// so the handle outcome is reported back here exactly once.
  ///
  /// The engine awaits *delivery + outcome*; it does **not** persist anything —
  /// the job queue, lease, and claim/delete are the caller's.
  ///
  /// Returns:
  /// - `Ok(())` — handled; the caller may delete the job.
  /// - [`EngineError::NotFound`] — no such entrypoint (the workflow is gone).
  ///   The caller drops the job; it is *not* retried.
  /// - [`EngineError::Handle`] — handled, but the handler returned an error.
  ///   Retriable; a persistently failing message is a poison candidate.
  /// - [`EngineError::Undelivered`] — the mailbox was gone before the message
  ///   could be enqueued. Transient — retry.
  /// - [`EngineError::Lost`] — enqueued, but the outcome never came back (shed,
  ///   or the actor died mid-handle). Transient — retry.
  ///
  /// **At-least-once, so duplicates are possible.** There is deliberately no
  /// timeout here — the durable caller wraps this call in `tokio::time::timeout`
  /// against its lease. A message that *is* handled but whose outcome doesn't
  /// arrive before the lease expires is re-invoked, so entrypoints reached this
  /// way must be idempotent / deduplicated.
  ///
  /// `id` is the run's [`CorrelationId`], exactly as on [`push`](Self::push) —
  /// minted at the trigger and propagated automatically downstream.
  ///
  /// [`MailboxTx::send`]: fuchsia_transport::MailboxTx::send
  pub async fn push_durable(
    &self,
    entrypoint: &ActorId,
    msg: Message,
    id: CorrelationId,
  ) -> Result<(), EngineError> {
    // Resolve the target and clone its mailbox sender, then drop the read guard
    // *before* any `.await`. Holding a `std` RwLock guard across an await would
    // make this future `!Send` (it could not be spawned) and would block
    // `add_node` / `remove_graph` for the whole handle. The Complete path needs
    // only the sender — not the health counter.
    let mailbox = {
      let state = self.router.read().map_err(|_| EngineError::Lock)?;
      let (mailbox, _health) = state
        .target(entrypoint)
        .ok_or_else(|| EngineError::NotFound(entrypoint.clone()))?;
      // Refcount bump on the mpsc sender so the guard can be released here.
      mailbox.clone()
    };

    // The Complete ack reports the handle outcome back through `rx` exactly
    // once; if it is dropped unreported (delivery shed / actor died mid-handle),
    // `rx` observes a closed channel — the documented retry-on-loss signal.
    let (tx, rx) = oneshot::channel::<Outcome>();
    mailbox
      .send(Delivery::with_correlation(msg, Ack::Complete(tx), id))
      .await
      .map_err(|_| EngineError::Undelivered)?;

    match rx.await {
      Ok(Ok(())) => Ok(()),
      Ok(Err(err)) => Err(EngineError::Handle(err)),
      Err(_recv) => Err(EngineError::Lost),
    }
  }
}

impl Default for Engine {
  fn default() -> Self {
    Self::new()
  }
}
