use thiserror::Error;

/// Errors that can occur when working with the component registry.
#[derive(Debug, Error)]
pub enum RegistryError {
  /// Component not found in the registry.
  #[error("component not found: {name}")]
  NotFound { name: String },

  /// Component version not found.
  #[error("component version not found: {name}@{version}")]
  VersionNotFound { name: String, version: String },

  /// IO error when reading/writing component files.
  #[error("io error: {0}")]
  Io(#[from] std::io::Error),

  /// Failed to parse manifest JSON.
  #[error("invalid manifest: {0}")]
  InvalidManifest(#[from] serde_json::Error),

  /// Digest mismatch when verifying component integrity.
  #[error("digest mismatch for {name}: expected {expected}, got {actual}")]
  DigestMismatch {
    name: String,
    expected: String,
    actual: String,
  },

  /// Component already exists in the registry.
  #[error("component already exists: {name}@{version}")]
  AlreadyExists { name: String, version: String },
}
