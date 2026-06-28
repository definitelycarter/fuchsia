use std::sync::{Arc, RwLock};

use fuchsia_actor::{ActorCapabilities, ActorConfig, ActorCreator, ActorId, Message};
use fuchsia_runtime::Runtime;
use fuchsia_transport::{Ack, Delivery};
use tokio::sync::Mutex;

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
    let (mailbox, health) = {
      let mut runtime = self.runtime.lock().await;
      runtime
        .spawn_with_caps(id.clone(), type_name, config, caps)
        .await?
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
}

impl Default for Engine {
  fn default() -> Self {
    Self::new()
  }
}
