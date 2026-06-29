//! Native builtin actors for fuchsia.
//!
//! These are plain Rust `Actor` impls — no WASM or Lua in the mix — registered
//! into an `ActorFactory` under canonical type names: `passthrough`, the
//! generic conditioning operators (`debounce` / `deadband` / `dedup`), and the
//! branching nodes (`if` / `switch`) that route over *named output ports*.

mod condition;
mod deadband;
mod debounce;
mod dedup;
mod if_;
mod passthrough;
mod switch;

pub use condition::{Condition, DeclCondition, Op, PreparedCondition};
pub use deadband::{Deadband, DeadbandCreator};
pub use debounce::{Debounce, DebounceCreator};
pub use dedup::{Dedup, DedupCreator};
pub use if_::{If, IfCreator};
pub use passthrough::{Passthrough, PassthroughCreator};
pub use switch::{Switch, SwitchCreator};

use bson::Document;
use fuchsia_actor::{ActorError, ActorFactory};
use serde::de::DeserializeOwned;

/// Register every builtin actor into `factory` under its canonical type name.
pub fn register(factory: &mut ActorFactory) {
  factory.register("passthrough", PassthroughCreator);
  factory.register("debounce", DebounceCreator);
  factory.register("deadband", DeadbandCreator);
  factory.register("dedup", DedupCreator);
  factory.register("if", IfCreator);
  factory.register("switch", SwitchCreator);
}

/// Deserialize an operator's typed config from a node's opaque `settings`
/// document. Malformed or missing settings surface as [`ActorError::Config`] at
/// construction — i.e. at provision time, not mid-stream.
pub(crate) fn from_settings<T: DeserializeOwned>(settings: &Document) -> Result<T, ActorError> {
  bson::de::deserialize_from_document(settings.clone())
    .map_err(|e| ActorError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use fuchsia_actor::{ActorCapabilities, ActorConfig};

  #[test]
  fn register_wires_every_builtin() {
    let mut factory = ActorFactory::new();
    register(&mut factory);
    for name in [
      "passthrough",
      "debounce",
      "deadband",
      "dedup",
      "if",
      "switch",
    ] {
      assert!(factory.contains(name), "missing builtin: {name}");
    }
    // Passthrough needs no config, so it's the one we can construct here; the
    // configured operators are exercised in their own modules.
    assert!(
      factory
        .create(
          "passthrough",
          &ActorConfig::default(),
          &ActorCapabilities::new()
        )
        .is_ok()
    );
  }
}
