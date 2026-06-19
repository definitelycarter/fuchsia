use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorCreator, ActorError, COMPONENT_ENV_KEY,
};
use wasmtime::Engine;
use wasmtime::component::Component;

use crate::actor::WasmActor;
use crate::host::WasmHost;

/// An [`ActorCreator`] registered once per *runtime* (`"wasm"`), not once per
/// component. It owns a catalog of compiled components keyed by identity; each
/// `create` reads the component id from [`ActorConfig::env`] (under
/// [`COMPONENT_ENV_KEY`]), looks it up, and builds a [`WasmActor`] wired to the
/// caller-supplied `emit`.
///
/// Generic over [`WasmHost`], so a product registers this with its own host
/// (its own capability imports) while reusing the loading/resolution logic.
/// The shared [`Engine`] is configured for the component model.
pub struct WasmActorCreator<H: WasmHost> {
  engine: Engine,
  host: Arc<H>,
  components: HashMap<String, Component>,
}

impl<H: WasmHost> WasmActorCreator<H> {
  /// Create a creator over a freshly built component-model [`Engine`].
  pub fn new(host: H) -> Result<Self, ActorError> {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)
      .map_err(|e| ActorError::Config(format!("build wasmtime engine: {e}")))?;
    Ok(Self::with_engine(engine, host))
  }

  /// Create a creator over a caller-provided [`Engine`] — use this to share one
  /// engine across creators, or to apply custom `Config` (fuel, epochs, …).
  /// The engine must have the component model enabled.
  pub fn with_engine(engine: Engine, host: H) -> Self {
    Self {
      engine,
      host: Arc::new(host),
      components: HashMap::new(),
    }
  }

  /// The shared engine — hand this to [`Component`] compilation done elsewhere.
  pub fn engine(&self) -> &Engine {
    &self.engine
  }

  /// Register an already-compiled component under `id`.
  pub fn insert_component(&mut self, id: impl Into<String>, component: Component) {
    self.components.insert(id.into(), component);
  }

  /// Compile a component from bytes and register it under `id`.
  pub fn insert_bytes(
    &mut self,
    id: impl Into<String>,
    bytes: impl AsRef<[u8]>,
  ) -> Result<(), ActorError> {
    let component = Component::new(&self.engine, bytes)
      .map_err(|e| ActorError::Config(format!("compile component: {e}")))?;
    self.insert_component(id, component);
    Ok(())
  }

  /// Compile a component from a `.wasm` file and register it under `id`.
  pub fn insert_path(
    &mut self,
    id: impl Into<String>,
    path: impl AsRef<Path>,
  ) -> Result<(), ActorError> {
    let path = path.as_ref();
    let component = Component::from_file(&self.engine, path)
      .map_err(|e| ActorError::Config(format!("compile component {}: {e}", path.display())))?;
    self.insert_component(id, component);
    Ok(())
  }

  /// Builder form of [`insert_bytes`](Self::insert_bytes).
  pub fn with_bytes(
    mut self,
    id: impl Into<String>,
    bytes: impl AsRef<[u8]>,
  ) -> Result<Self, ActorError> {
    self.insert_bytes(id, bytes)?;
    Ok(self)
  }

  /// Builder form of [`insert_path`](Self::insert_path).
  pub fn with_path(
    mut self,
    id: impl Into<String>,
    path: impl AsRef<Path>,
  ) -> Result<Self, ActorError> {
    self.insert_path(id, path)?;
    Ok(self)
  }
}

impl<H: WasmHost> ActorCreator for WasmActorCreator<H> {
  fn create(
    &self,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    let id = config
      .env
      .get(COMPONENT_ENV_KEY)
      .ok_or_else(|| ActorError::Config(format!("missing `{COMPONENT_ENV_KEY}` in env")))?;
    let component = self
      .components
      .get(id)
      .ok_or_else(|| ActorError::Config(format!("unknown component: {id}")))?;

    // `Component`/`Engine` clones are refcount bumps; each actor gets its own
    // store at setup but shares the compiled artifact and engine.
    Ok(Box::new(WasmActor::new(
      self.engine.clone(),
      component.clone(),
      Arc::clone(&self.host),
      caps.emit(),
    )))
  }
}
