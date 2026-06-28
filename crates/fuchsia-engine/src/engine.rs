use std::sync::{Arc, RwLock};

use fuchsia_actor::{ActorCapabilities, ActorConfig, ActorCreator, ActorId, Message};
use fuchsia_runtime::Runtime;
use fuchsia_transport::{Ack, Delivery, Outcome};
use tokio::sync::{Mutex, oneshot};

use crate::error::EngineError;
use crate::router::{RoutedEmit, RouterState};

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
}

impl Engine {
  pub fn new() -> Self {
    Self {
      runtime: Mutex::new(Runtime::new()),
      router: Arc::new(RwLock::new(RouterState::default())),
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
    let (mailbox, health) = {
      let mut runtime = self.runtime.lock().await;
      runtime.commit(spawning)?
    };

    self
      .router
      .write()
      .map_err(|_| EngineError::Lock)?
      .register(id, mailbox, health);
    Ok(())
  }

  /// Add a directed edge: `from`'s emissions flow to `to`'s mailbox.
  pub fn add_edge(&self, from: ActorId, to: ActorId) -> Result<(), EngineError> {
    self
      .router
      .write()
      .map_err(|_| EngineError::Lock)?
      .add_edge(from, to);
    Ok(())
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
  pub fn push(&self, entrypoint: &ActorId, msg: Message) -> Result<(), EngineError> {
    let state = self.router.read().map_err(|_| EngineError::Lock)?;
    let (mailbox, health) = state
      .target(entrypoint)
      .ok_or_else(|| EngineError::NotFound(entrypoint.clone()))?;
    let _ = mailbox.offer(Delivery::new(msg, Ack::Health(health.clone())));
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
  /// [`MailboxTx::send`]: fuchsia_transport::MailboxTx::send
  pub async fn push_durable(&self, entrypoint: &ActorId, msg: Message) -> Result<(), EngineError> {
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
      .send(Delivery::new(msg, Ack::Complete(tx)))
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
