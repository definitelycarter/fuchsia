use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::error::RegistryError;
use crate::manifest::ComponentManifest;

/// A resolved component ready for use.
#[derive(Debug, Clone)]
pub struct InstalledComponent {
  /// The component manifest.
  pub manifest: ComponentManifest,

  /// Path to the component.wasm file.
  pub wasm_path: PathBuf,

  /// Path to the component directory (contains manifest, wasm, readme, assets).
  pub component_dir: PathBuf,
}

/// Registry for managing installed components.
#[async_trait]
pub trait ComponentRegistry: Send + Sync {
  /// Get an installed component by name, optionally at a specific version.
  /// If version is None, returns the latest installed version.
  async fn get(
    &self,
    name: &str,
    version: Option<&str>,
  ) -> Result<Option<InstalledComponent>, RegistryError>;

  /// Install a component from a package directory or archive.
  /// The package should contain manifest.json and component.wasm.
  async fn install(&self, package_path: &Path) -> Result<InstalledComponent, RegistryError>;

  /// List all installed components.
  async fn list(&self) -> Result<Vec<ComponentManifest>, RegistryError>;

  /// Remove an installed component.
  async fn remove(&self, name: &str, version: &str) -> Result<(), RegistryError>;
}
