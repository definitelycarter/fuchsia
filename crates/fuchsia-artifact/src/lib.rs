//! Fuchsia Artifact
//!
//! This crate provides the artifact storage trait and implementations for Fuchsia.
//! Artifacts are binary blobs (images, files, large data) that are stored separately
//! from workflow execution data.
//!
//! The [`Store`] trait defines the platform/backend layer for artifact storage.
//! Implementations handle the actual storage (filesystem, S3, etc.) while the
//! engine translates artifact IDs to storage keys.
//!
//! The trait uses async streaming for efficient handling of large files.

mod fs;

pub use fs::FsStore;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;

/// A boxed stream of bytes for artifact data.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, Error>> + Send>>;

/// Error type for artifact storage operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  /// The requested artifact was not found.
  #[error("artifact not found: {0}")]
  NotFound(String),

  /// An I/O error occurred.
  #[error("io error: {0}")]
  Io(#[from] std::io::Error),
}

/// Artifact storage trait.
///
/// Implementations provide the actual storage backend (filesystem, S3, etc.).
/// The engine is responsible for translating artifact IDs to storage keys.
#[async_trait]
pub trait Store: Send + Sync {
  /// Retrieve an artifact by key.
  ///
  /// Returns a stream of bytes for efficient handling of large files.
  async fn get(&self, key: &str) -> Result<ByteStream, Error>;

  /// Store an artifact.
  ///
  /// Accepts a stream of bytes for efficient handling of large files.
  async fn put(&self, key: &str, data: ByteStream, content_type: &str) -> Result<(), Error>;

  /// Delete an artifact by key.
  async fn delete(&self, key: &str) -> Result<(), Error>;
}
