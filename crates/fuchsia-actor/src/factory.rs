use std::collections::{BTreeMap, HashMap};

use bson::Document;

use crate::actor::{Actor, ActorCapabilities};
use crate::error::ActorError;

/// Reserved [`ActorConfig::env`] key under which a per-runtime guest creator
/// (wasm/lua) finds the identity of the component/script to load.
///
/// A guest runtime registers *one* creator per runtime kind (e.g. `"wasm"`,
/// `"lua"`), not one per component; the specific component a node runs is named
/// here. It lives in `env` (host-curated) rather than `settings` (guest-opaque)
/// because the *host* — the creator — resolves it, never the guest. The host
/// writes a `Component` node's identifier here; the creator reads it.
pub const COMPONENT_ENV_KEY: &str = "component";

/// Per-instance configuration handed to an actor at construction, split by
/// *who consumes it*:
///
/// - `env` is **host-understood** — the host curates the environment an actor
///   sees, so a sandboxed actor sees only what's declared here, never the real
///   host environment. The host must be able to read it, so it's concrete.
/// - `settings` is **opaque to the host** and meaningful only to the actor — an
///   operator's delay, a component's schema-validated config. The actor
///   deserializes its own typed view from it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorConfig {
  pub env: BTreeMap<String, String>,
  pub settings: Document,
}

/// The output ports a node advertises — its *interface*, computed from the
/// node's type plus its config. Used by the engine to validate edges at wiring
/// time and by an editor to draw a node's outputs.
///
/// A port is a named output *on* a node, not a node itself; the names ride on
/// the edges that leave them. See the named-output-ports design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPorts {
  /// A fixed, declarable set of ports — `if` → `["true", "false"]`,
  /// `passthrough` → `["out"]`; the config-*derived* case is the same variant
  /// computed from settings (`switch` → its `cases` + `["default"]`). The
  /// engine validates edges against this set.
  Fixed(Vec<String>),
  /// Ports that exist only at emit time and so cannot be validated — a
  /// Lua/Wasm/JS script node, whose ports are whatever its code emits on. The
  /// honest answer for guests, not a gap; the engine accepts any port.
  Dynamic,
}

pub trait ActorCreator: Send + Sync + 'static {
  /// Build the actor, storing whatever subset of `caps` it uses. The signature
  /// is uniform (dyn-dispatched), so every creator is *offered* the full bundle
  /// — what it keeps is what it can do.
  fn create(
    &self,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError>;

  /// Advertise this node type's output ports for the given config. The default
  /// is [`OutputPorts::Dynamic`] — ports validated nowhere — so every existing
  /// creator compiles untouched; a node with a known, fixed interface (`if`,
  /// `switch`, `passthrough`) overrides this to return [`OutputPorts::Fixed`].
  fn output_ports(&self, _config: &ActorConfig) -> OutputPorts {
    OutputPorts::Dynamic
  }
}

pub struct ActorFactory {
  creators: HashMap<String, Box<dyn ActorCreator>>,
}

impl ActorFactory {
  pub fn new() -> Self {
    Self {
      creators: HashMap::new(),
    }
  }

  pub fn register(&mut self, type_name: impl Into<String>, creator: impl ActorCreator) {
    self.creators.insert(type_name.into(), Box::new(creator));
  }

  /// Register a closure directly as a node type — none of the `struct`,
  /// `ActorCreator`, `impl Actor` triple. `builder` is called once per node
  /// instance with that node's `config` and `caps` (pull `caps.emit()` and hand
  /// it to [`from_fn`](crate::from_fn)), and returns the built actor. Sugar over
  /// a [`FnCreator`], so the node routes, validates, and tears down like any
  /// other.
  ///
  /// The node's output ports default to [`OutputPorts::Dynamic`] — a closure's
  /// ports are whatever it emits on. Use [`register_fn_with_ports`](Self::register_fn_with_ports)
  /// to declare a fixed, validatable interface instead.
  pub fn register_fn<F>(&mut self, type_name: impl Into<String>, builder: F)
  where
    F: Fn(&ActorConfig, &ActorCapabilities) -> Box<dyn Actor> + Send + Sync + 'static,
  {
    self.register(type_name, FnCreator::new(builder));
  }

  /// Like [`register_fn`](Self::register_fn), but the node declares a fixed set
  /// of output `ports` ([`OutputPorts::Fixed`]) so the engine can validate edges
  /// against them — for a closure node with a known interface.
  pub fn register_fn_with_ports<F>(
    &mut self,
    type_name: impl Into<String>,
    ports: Vec<String>,
    builder: F,
  ) where
    F: Fn(&ActorConfig, &ActorCapabilities) -> Box<dyn Actor> + Send + Sync + 'static,
  {
    self.register(type_name, FnCreator::with_ports(ports, builder));
  }

  pub fn create(
    &self,
    type_name: &str,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    self
      .creators
      .get(type_name)
      .ok_or_else(|| ActorError::UnknownType(type_name.to_owned()))?
      .create(config, caps)
  }

  /// The output ports a registered type advertises for `config` — the engine
  /// uses this to validate edges. Resolving the creator by name is the same
  /// lookup [`create`](Self::create) does, so the two stay in lockstep.
  pub fn output_ports(
    &self,
    type_name: &str,
    config: &ActorConfig,
  ) -> Result<OutputPorts, ActorError> {
    Ok(
      self
        .creators
        .get(type_name)
        .ok_or_else(|| ActorError::UnknownType(type_name.to_owned()))?
        .output_ports(config),
    )
  }

  pub fn contains(&self, type_name: &str) -> bool {
    self.creators.contains_key(type_name)
  }
}

impl Default for ActorFactory {
  fn default() -> Self {
    Self::new()
  }
}

/// An [`ActorCreator`] backed by a closure — the mechanism behind
/// [`ActorFactory::register_fn`]. Holds the per-instance `builder` and the
/// node's declared [`OutputPorts`]; `create` just invokes the builder, so a
/// closure node is provisioned, routed, and torn down on the same path as any
/// hand-written creator.
pub struct FnCreator<F> {
  builder: F,
  ports: OutputPorts,
}

impl<F> FnCreator<F>
where
  F: Fn(&ActorConfig, &ActorCapabilities) -> Box<dyn Actor> + Send + Sync + 'static,
{
  /// A creator whose node has [`OutputPorts::Dynamic`] ports — the honest
  /// default for a closure, whose ports are whatever it emits on.
  pub fn new(builder: F) -> Self {
    Self {
      builder,
      ports: OutputPorts::Dynamic,
    }
  }

  /// A creator that declares a fixed set of output `ports`
  /// ([`OutputPorts::Fixed`]), for a closure node with a known interface the
  /// engine can validate edges against.
  pub fn with_ports(ports: Vec<String>, builder: F) -> Self {
    Self {
      builder,
      ports: OutputPorts::Fixed(ports),
    }
  }
}

impl<F> ActorCreator for FnCreator<F>
where
  F: Fn(&ActorConfig, &ActorCapabilities) -> Box<dyn Actor> + Send + Sync + 'static,
{
  fn create(
    &self,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok((self.builder)(config, caps))
  }

  fn output_ports(&self, _config: &ActorConfig) -> OutputPorts {
    // Cloned, not a refcount bump: `output_ports` is a wiring-time call (not the
    // per-message path), and the trait hands back an owned descriptor. `Dynamic`
    // clones for free; `Fixed` copies the small declared port list.
    self.ports.clone()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::actor::{Actor, ActorContext, Message};
  use async_trait::async_trait;

  struct EchoActor;

  // Only `handle` — relies on the trait's default no-op `setup`/`teardown`.
  #[async_trait]
  impl Actor for EchoActor {
    async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
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

  #[test]
  fn create_registered_type() {
    let mut factory = ActorFactory::new();
    factory.register("echo", EchoCreator);
    assert!(
      factory
        .create("echo", &ActorConfig::default(), &ActorCapabilities::new())
        .is_ok()
    );
  }

  #[test]
  fn unknown_type_returns_error() {
    let factory = ActorFactory::new();
    let err = factory
      .create(
        "unknown",
        &ActorConfig::default(),
        &ActorCapabilities::new(),
      )
      .err()
      .unwrap();
    assert!(matches!(err, ActorError::UnknownType(t) if t == "unknown"));
  }

  #[test]
  fn contains_reflects_registration() {
    let mut factory = ActorFactory::new();
    assert!(!factory.contains("echo"));
    factory.register("echo", EchoCreator);
    assert!(factory.contains("echo"));
  }

  #[test]
  fn output_ports_defaults_to_dynamic() {
    assert_eq!(
      EchoCreator.output_ports(&ActorConfig::default()),
      OutputPorts::Dynamic
    );
  }

  struct FixedPortsCreator;

  impl ActorCreator for FixedPortsCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(EchoActor))
    }

    fn output_ports(&self, _config: &ActorConfig) -> OutputPorts {
      OutputPorts::Fixed(vec!["true".to_owned(), "false".to_owned()])
    }
  }

  #[test]
  fn output_ports_override_is_observed() {
    assert_eq!(
      FixedPortsCreator.output_ports(&ActorConfig::default()),
      OutputPorts::Fixed(vec!["true".to_owned(), "false".to_owned()])
    );
  }

  // --- register_fn ------------------------------------------------------------

  use crate::actor::Emit;
  use std::sync::{Arc, Mutex};

  /// Test sink recording the `(port, type)` of every emission.
  struct Recorder(Arc<Mutex<Vec<(String, String)>>>);
  impl Emit for Recorder {
    fn emit_to(&self, port: &str, msg: Message) {
      self.0.lock().unwrap().push((port.to_owned(), msg.type_));
    }
  }

  #[tokio::test]
  async fn register_fn_builds_a_working_closure_node() {
    let mut factory = ActorFactory::new();
    // A `tap` node type defined as just a closure: pull `emit` from caps and
    // forward each message through `from_fn`.
    factory.register_fn("tap", |_config, caps| {
      crate::from_fn(caps.emit(), |_ctx, msg, emit| async move {
        emit.emit(msg);
        Ok(())
      })
    });

    let log = Arc::new(Mutex::new(Vec::new()));
    let caps = ActorCapabilities::new().with_emit(Arc::new(Recorder(log.clone())));
    let mut actor = factory
      .create("tap", &ActorConfig::default(), &caps)
      .unwrap();
    actor
      .handle(&ActorContext::new("e", "n", "t"), Message::empty("reading"))
      .await
      .unwrap();

    assert_eq!(
      *log.lock().unwrap(),
      vec![("out".to_owned(), "reading".to_owned())]
    );
  }

  #[test]
  fn register_fn_defaults_to_dynamic_ports() {
    let mut factory = ActorFactory::new();
    factory.register_fn("tap", |_config, caps| {
      crate::from_fn(caps.emit(), |_ctx, msg, emit| async move {
        emit.emit(msg);
        Ok(())
      })
    });
    assert_eq!(
      factory
        .output_ports("tap", &ActorConfig::default())
        .unwrap(),
      OutputPorts::Dynamic
    );
  }

  #[test]
  fn register_fn_with_ports_declares_a_fixed_interface() {
    let mut factory = ActorFactory::new();
    factory.register_fn_with_ports(
      "split",
      vec!["left".to_owned(), "right".to_owned()],
      |_config, caps| {
        crate::from_fn(caps.emit(), |_ctx, msg, emit| async move {
          emit.emit_to("left", msg);
          Ok(())
        })
      },
    );
    assert_eq!(
      factory
        .output_ports("split", &ActorConfig::default())
        .unwrap(),
      OutputPorts::Fixed(vec!["left".to_owned(), "right".to_owned()])
    );
  }
}
