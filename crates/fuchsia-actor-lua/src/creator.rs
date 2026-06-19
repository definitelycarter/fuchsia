use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorCreator, ActorError, COMPONENT_ENV_KEY,
};

use crate::actor::LuaActor;
use crate::host::LuaHost;

/// An [`ActorCreator`] registered once per *runtime* (`"lua"`), not once per
/// script. It owns a catalog of script sources keyed by identity; each `create`
/// reads the script id from [`ActorConfig::env`] (under [`COMPONENT_ENV_KEY`]),
/// looks it up, and builds a [`LuaActor`] wired to the caller-supplied `emit`.
///
/// Generic over [`LuaHost`], so a product registers this with its own host
/// (its own capability globals) while reusing the resolution logic.
pub struct LuaActorCreator<H: LuaHost> {
  host: Arc<H>,
  scripts: HashMap<String, Arc<String>>,
}

impl<H: LuaHost> LuaActorCreator<H> {
  pub fn new(host: H) -> Self {
    Self {
      host: Arc::new(host),
      scripts: HashMap::new(),
    }
  }

  /// Register inline script source under `id`.
  pub fn insert_source(&mut self, id: impl Into<String>, source: impl Into<String>) {
    self.scripts.insert(id.into(), Arc::new(source.into()));
  }

  /// Read a script from a `.lua` file and register it under `id`.
  pub fn insert_path(
    &mut self,
    id: impl Into<String>,
    path: impl AsRef<Path>,
  ) -> Result<(), ActorError> {
    let path = path.as_ref();
    let source = std::fs::read_to_string(path)
      .map_err(|e| ActorError::Config(format!("read lua source {}: {e}", path.display())))?;
    self.insert_source(id, source);
    Ok(())
  }

  /// Builder form of [`insert_source`](Self::insert_source).
  pub fn with_source(mut self, id: impl Into<String>, source: impl Into<String>) -> Self {
    self.insert_source(id, source);
    self
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

impl<H: LuaHost> ActorCreator for LuaActorCreator<H> {
  fn create(
    &self,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    let id = config
      .env
      .get(COMPONENT_ENV_KEY)
      .ok_or_else(|| ActorError::Config(format!("missing `{COMPONENT_ENV_KEY}` in env")))?;
    let source = self
      .scripts
      .get(id)
      .ok_or_else(|| ActorError::Config(format!("unknown script: {id}")))?;

    Ok(Box::new(LuaActor::new(
      Arc::clone(source),
      Arc::clone(&self.host),
      caps.emit(),
    )))
  }
}
