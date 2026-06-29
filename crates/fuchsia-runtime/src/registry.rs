use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fuchsia_actor::ActorId;
use fuchsia_transport::{Health, MailboxTx};

/// A running actor's address book entry: its identity, type, the mailbox to
/// deliver into, its health counters, and a stop flag shared with its
/// supervisor.
///
/// The `stopping` flag is how the per-node supervisor (which owns the actor's
/// `JoinHandle`) tells an *intentional* shutdown apart from a *death*. A
/// deliberate stop ([`ActorRegistry`] removal via `Runtime::stop` /
/// `remove_graph`) sets it before the mailbox sender drops, so when the run
/// loop then exits on its closed `rx` the supervisor reads a clean stop and
/// does **not** count a death. An exit with the flag unset — a panic, or
/// senders vanishing without an explicit stop — is a death.
#[derive(Debug, Clone)]
pub struct ActorHandle {
  id: ActorId,
  actor_type: String,
  mailbox: MailboxTx,
  health: Arc<Health>,
  // Shared with the supervisor (an `Arc` so both ends see the same flag); a
  // refcount bump on insert, not a re-allocation.
  stopping: Arc<AtomicBool>,
}

impl ActorHandle {
  pub fn new(
    id: ActorId,
    actor_type: impl Into<String>,
    mailbox: MailboxTx,
    health: Arc<Health>,
  ) -> Self {
    Self::new_sharing(
      id,
      actor_type,
      mailbox,
      health,
      Arc::new(AtomicBool::new(false)),
    )
  }

  /// Like [`new`](Self::new), but reuses a **caller-provided** `stopping` flag
  /// rather than minting a fresh one — so a re-inserted handle (a *revived*
  /// restart-supervised node) shares the *same* flag its supervisor already
  /// reads, keeping `stop`/`mark_stopping` honest across the revive. A
  /// refcount bump of the shared flag.
  pub fn new_sharing(
    id: ActorId,
    actor_type: impl Into<String>,
    mailbox: MailboxTx,
    health: Arc<Health>,
    stopping: Arc<AtomicBool>,
  ) -> Self {
    Self {
      id,
      actor_type: actor_type.into(),
      mailbox,
      health,
      stopping,
    }
  }

  pub fn id(&self) -> &ActorId {
    &self.id
  }

  pub fn actor_type(&self) -> &str {
    &self.actor_type
  }

  pub fn mailbox(&self) -> &MailboxTx {
    &self.mailbox
  }

  pub fn health(&self) -> &Arc<Health> {
    &self.health
  }

  /// A handle to this node's stop flag, shared with its supervisor. The
  /// supervisor reads it on the actor task's exit to tell an intentional stop
  /// from a death.
  pub(crate) fn stopping(&self) -> Arc<AtomicBool> {
    // Refcount bump of the shared flag so the supervisor can hold its own
    // handle to it.
    Arc::clone(&self.stopping)
  }

  /// Mark this node as intentionally stopping, so its supervisor treats the
  /// imminent run-loop exit as a clean shutdown rather than a death. Set before
  /// the mailbox sender is dropped.
  pub(crate) fn mark_stopping(&self) {
    self.stopping.store(true, Ordering::SeqCst);
  }
}

pub struct ActorRegistry {
  actors: HashMap<ActorId, ActorHandle>,
}

impl ActorRegistry {
  pub fn new() -> Self {
    Self {
      actors: HashMap::new(),
    }
  }

  pub fn insert(&mut self, handle: ActorHandle) {
    self.actors.insert(handle.id.clone(), handle);
  }

  pub fn get(&self, id: &ActorId) -> Option<&ActorHandle> {
    self.actors.get(id)
  }

  pub fn remove(&mut self, id: &ActorId) -> Option<ActorHandle> {
    self.actors.remove(id)
  }

  pub fn contains(&self, id: &ActorId) -> bool {
    self.actors.contains_key(id)
  }
}

impl Default for ActorRegistry {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use fuchsia_transport::mailbox;

  fn echo_handle(id: &str) -> ActorHandle {
    let (tx, _rx) = mailbox(1);
    ActorHandle::new(ActorId::new(id), "echo", tx, Arc::new(Health::default()))
  }

  #[test]
  fn insert_and_get() {
    let mut registry = ActorRegistry::new();
    registry.insert(echo_handle("van-gps-01"));
    let handle = registry.get(&ActorId::new("van-gps-01")).unwrap();
    assert_eq!(handle.actor_type(), "echo");
  }

  #[test]
  fn get_missing_returns_none() {
    let registry = ActorRegistry::new();
    assert!(registry.get(&ActorId::new("missing")).is_none());
  }

  #[test]
  fn remove_returns_handle() {
    let mut registry = ActorRegistry::new();
    registry.insert(echo_handle("van-gps-01"));
    let removed = registry.remove(&ActorId::new("van-gps-01")).unwrap();
    assert_eq!(removed.id(), &ActorId::new("van-gps-01"));
    assert!(!registry.contains(&ActorId::new("van-gps-01")));
  }

  #[test]
  fn remove_missing_returns_none() {
    let mut registry = ActorRegistry::new();
    assert!(registry.remove(&ActorId::new("missing")).is_none());
  }

  #[test]
  fn contains_reflects_insertion() {
    let mut registry = ActorRegistry::new();
    assert!(!registry.contains(&ActorId::new("van-gps-01")));
    registry.insert(echo_handle("van-gps-01"));
    assert!(registry.contains(&ActorId::new("van-gps-01")));
  }

  #[test]
  fn insert_overwrites_existing() {
    let mut registry = ActorRegistry::new();
    registry.insert(echo_handle("van-gps-01"));
    let (tx, _rx) = mailbox(1);
    registry.insert(ActorHandle::new(
      ActorId::new("van-gps-01"),
      "gps",
      tx,
      Arc::new(Health::default()),
    ));
    let handle = registry.get(&ActorId::new("van-gps-01")).unwrap();
    assert_eq!(handle.actor_type(), "gps");
  }

  #[test]
  fn multiple_instances_of_same_type() {
    let mut registry = ActorRegistry::new();
    registry.insert(echo_handle("instance-1"));
    registry.insert(echo_handle("instance-2"));
    assert!(registry.contains(&ActorId::new("instance-1")));
    assert!(registry.contains(&ActorId::new("instance-2")));
  }
}
