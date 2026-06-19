use std::collections::HashMap;
use std::sync::Arc;

use fuchsia_actor::ActorId;
use fuchsia_transport::{Health, MailboxTx};

/// A running actor's address book entry: its identity, type, the mailbox to
/// deliver into, and its health counters.
#[derive(Debug, Clone)]
pub struct ActorHandle {
  id: ActorId,
  actor_type: String,
  mailbox: MailboxTx,
  health: Arc<Health>,
}

impl ActorHandle {
  pub fn new(
    id: ActorId,
    actor_type: impl Into<String>,
    mailbox: MailboxTx,
    health: Arc<Health>,
  ) -> Self {
    Self {
      id,
      actor_type: actor_type.into(),
      mailbox,
      health,
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
