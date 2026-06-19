use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use fuchsia_actor::{ActorId, Emit, Message};
use fuchsia_transport::{Ack, Delivery, Health, MailboxTx};

/// The engine's live routing table: who the successors of each node are, and
/// how to reach every node's mailbox. Mutable so graphs can be added or torn
/// down without re-instantiating actors (lookup, not baked wiring).
#[derive(Default)]
pub(crate) struct RouterState {
  edges: HashMap<ActorId, Vec<ActorId>>,
  targets: HashMap<ActorId, (MailboxTx, Arc<Health>)>,
}

impl RouterState {
  pub(crate) fn register(&mut self, id: ActorId, mailbox: MailboxTx, health: Arc<Health>) {
    self.targets.insert(id, (mailbox, health));
  }

  pub(crate) fn add_edge(&mut self, from: ActorId, to: ActorId) {
    self.edges.entry(from).or_default().push(to);
  }

  pub(crate) fn target(&self, id: &ActorId) -> Option<&(MailboxTx, Arc<Health>)> {
    self.targets.get(id)
  }

  pub(crate) fn ids_in_group(&self, group: &str) -> Vec<ActorId> {
    self
      .targets
      .keys()
      .filter(|id| id.group() == group)
      .cloned()
      .collect()
  }

  /// Drop a group's targets and the edges it owns (an edge belongs to its
  /// source node's group). Cross-group edges *into* this group are left to
  /// resolve to nothing — a graceful drop, handled by `route`.
  pub(crate) fn remove_group(&mut self, group: &str) {
    self.targets.retain(|id, _| id.group() != group);
    self.edges.retain(|from, _| from.group() != group);
  }

  /// Deliver `msg` to each successor of `source`. Channel transport, so a full
  /// mailbox sheds (at-most-once); the per-target `Health` records the outcome.
  fn route(&self, source: &ActorId, msg: Message) {
    let Some(successors) = self.edges.get(source) else {
      return;
    };
    for to in successors {
      if let Some((tx, health)) = self.targets.get(to) {
        let ack = Ack::Health(health.clone());
        let _ = tx.offer(Delivery::new(msg.clone(), ack));
      }
    }
  }
}

/// The `emit` sink handed to one actor. On emit, it looks the source's
/// successors up in the shared router and delivers — the actor stays
/// neighbor-ignorant; the engine owns the addressing.
pub(crate) struct RoutedEmit {
  pub(crate) source: ActorId,
  pub(crate) router: Arc<RwLock<RouterState>>,
}

impl Emit for RoutedEmit {
  fn emit(&self, msg: Message) {
    // A poisoned router lock means a prior panic mid-mutation; drop rather
    // than propagate (emit is infallible and best-effort on this path).
    if let Ok(state) = self.router.read() {
      state.route(&self.source, msg);
    }
  }
}
