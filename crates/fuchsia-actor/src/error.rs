use thiserror::Error;

#[derive(Debug, Error)]
pub enum ActorError {
  #[error("setup failed: {0}")]
  Setup(String),
  #[error("handle failed: {0}")]
  Handle(String),
  #[error("teardown failed: {0}")]
  Teardown(String),
  #[error("unknown actor type: {0}")]
  UnknownType(String),
  #[error("invalid config: {0}")]
  Config(String),
  #[error("state write failed: {0}")]
  StateWrite(String),
}
