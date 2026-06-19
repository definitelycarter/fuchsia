use fuchsia_engine::EngineError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProvisionerError {
  #[error("engine: {0}")]
  Engine(#[from] EngineError),
}
