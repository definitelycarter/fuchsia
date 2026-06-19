/// Stable, globally-unique identity for a running actor — and its routing
/// address.
///
/// The `group` is part of the address: the same local id in two groups (e.g.
/// each entity's `debounce`) is two distinct actors that route independently.
/// Local ids only need to be unique *within* a group; the group namespaces
/// them globally.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorId {
  group: String,
  id: String,
}

/// Group for actors not scoped to a particular entity or workflow.
const DEFAULT_GROUP: &str = "default";

impl ActorId {
  /// An id in the default group.
  pub fn new(id: impl Into<String>) -> Self {
    Self {
      group: DEFAULT_GROUP.to_owned(),
      id: id.into(),
    }
  }

  /// An id scoped to a group — an entity or workflow instance. The group is
  /// the unit `remove_graph` tears down, and it disambiguates same-named nodes
  /// across instances.
  pub fn scoped(group: impl Into<String>, id: impl Into<String>) -> Self {
    Self {
      group: group.into(),
      id: id.into(),
    }
  }

  pub fn group(&self) -> &str {
    &self.group
  }

  pub fn id(&self) -> &str {
    &self.id
  }
}

impl std::fmt::Display for ActorId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    if self.group == DEFAULT_GROUP {
      f.write_str(&self.id)
    } else {
      write!(f, "{}/{}", self.group, self.id)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_group_displays_bare_id() {
    let id = ActorId::new("van-gps-01");
    assert_eq!(id.to_string(), "van-gps-01");
    assert_eq!(id.group(), "default");
    assert_eq!(id.id(), "van-gps-01");
  }

  #[test]
  fn scoped_displays_group_and_id() {
    let id = ActorId::scoped("entity:fridge", "debounce");
    assert_eq!(id.to_string(), "entity:fridge/debounce");
    assert_eq!(id.group(), "entity:fridge");
  }

  #[test]
  fn group_is_part_of_identity() {
    // Same local id in different groups is a different actor.
    assert_ne!(ActorId::scoped("g1", "a"), ActorId::scoped("g2", "a"));
    assert_ne!(ActorId::scoped("g1", "a"), ActorId::new("a"));
    // Same group + id is equal.
    assert_eq!(ActorId::scoped("g1", "a"), ActorId::scoped("g1", "a"));
    assert_eq!(ActorId::new("a"), ActorId::new("a"));
  }
}
