use fuchsia_actor::{ActorError, ActorId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
  #[error("actor not found: {0}")]
  ActorNotFound(ActorId),
  #[error("actor already running: {0}")]
  AlreadyRunning(ActorId),
  #[error("actor error: {0}")]
  Actor(#[from] ActorError),
  #[error("send failed: {0}")]
  Send(String),
  #[error("registry lock poisoned")]
  Lock,
}
