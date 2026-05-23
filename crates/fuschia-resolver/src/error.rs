use thiserror::Error;

/// Errors that can occur during workflow resolution.
#[derive(Debug, Error)]
pub enum ResolveError {
  /// Component not found in the registry.
  #[error("component not found: {name}")]
  ComponentNotFound { name: String },

  /// Component version not found.
  #[error("component version not found: {name}@{version}")]
  ComponentVersionNotFound { name: String, version: String },

  /// Invalid edge reference (node doesn't exist).
  #[error("invalid edge: node '{node_id}' does not exist")]
  InvalidEdge { node_id: String },

  /// Duplicate node ID.
  #[error("duplicate node id: {node_id}")]
  DuplicateNodeId { node_id: String },

  /// Workflow has no entry points (no nodes without incoming edges).
  #[error("workflow has no entry points")]
  NoEntryPoints,

  /// Cycle detected in the workflow graph.
  #[error("cycle detected in workflow graph")]
  CycleDetected,

  /// Task not found in component manifest.
  #[error("task '{task_name}' not found in component '{component}'")]
  TaskNotFound {
    component: String,
    task_name: String,
  },

  /// Registry error while looking up component.
  #[error("registry error: {0}")]
  Registry(#[from] fuschia_component_registry::RegistryError),
}
