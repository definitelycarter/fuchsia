use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkflowError {
  #[error("node not found: {0}")]
  NodeNotFound(String),

  #[error("edge references unknown node: from={from}, to={to}")]
  InvalidEdge { from: String, to: String },

  #[error("no entry points found (all nodes have incoming edges)")]
  NoEntryPoints,

  #[error("component not found or failed to resolve: {0}")]
  ComponentResolutionFailed(String),

  #[error("invalid component digest: expected {expected}, got {actual}")]
  DigestMismatch { expected: String, actual: String },
}
