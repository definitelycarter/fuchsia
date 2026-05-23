use std::path::PathBuf;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::{ByteStream, Error, Store};

/// Filesystem-based artifact store.
///
/// Stores artifacts as files on the local filesystem. Each artifact is stored
/// at `{base_path}/{key}`. Parent directories are created automatically.
pub struct FsStore {
  base_path: PathBuf,
}

impl FsStore {
  /// Create a new filesystem store with the given base path.
  pub fn new(base_path: impl Into<PathBuf>) -> Self {
    Self {
      base_path: base_path.into(),
    }
  }

  fn key_to_path(&self, key: &str) -> PathBuf {
    self.base_path.join(key)
  }
}

#[async_trait]
impl Store for FsStore {
  async fn get(&self, key: &str) -> Result<ByteStream, Error> {
    let path = self.key_to_path(key);
    let file = File::open(&path).await.map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        Error::NotFound(key.to_string())
      } else {
        Error::Io(e)
      }
    })?;
    let stream = ReaderStream::new(file).map(|r| r.map_err(Error::Io));
    Ok(Box::pin(stream))
  }

  async fn put(&self, key: &str, data: ByteStream, _content_type: &str) -> Result<(), Error> {
    let path = self.key_to_path(key);

    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent).await?;
    }

    let mut file = File::create(path).await?;
    let mut stream = std::pin::pin!(data);

    while let Some(chunk) = stream.next().await {
      let bytes = chunk?;
      file.write_all(&bytes).await?;
    }

    file.flush().await?;
    Ok(())
  }

  async fn delete(&self, key: &str) -> Result<(), Error> {
    let path = self.key_to_path(key);
    fs::remove_file(&path).await.map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        Error::NotFound(key.to_string())
      } else {
        Error::Io(e)
      }
    })
  }
}
