use fuchsia_actor::ActorId;
use fuchsia_runtime::RuntimeError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
  #[error("runtime: {0}")]
  Runtime(#[from] RuntimeError),
  #[error("router lock poisoned")]
  Lock,
  #[error("node not found: {0}")]
  NotFound(ActorId),
}
