use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::fs;

use crate::error::RegistryError;
use crate::manifest::ComponentManifest;
use crate::registry::{ComponentRegistry, InstalledComponent};

/// Filesystem-based component registry.
///
/// Components are stored in a directory structure:
/// ```text
/// {root}/
/// └── my-org--sentiment-analysis--1.0.0/
///     ├── manifest.json
///     ├── component.wasm
///     ├── README.md (optional)
///     └── assets/ (optional)
/// ```
pub struct FsComponentRegistry {
  root: PathBuf,
}

impl FsComponentRegistry {
  /// Create a new filesystem registry at the given root path.
  pub fn new(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  /// Get the root directory of the registry.
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Parse a directory name into (name, version).
  /// Example: "my-org--sentiment-analysis--1.0.0" -> ("my-org/sentiment-analysis", "1.0.0")
  fn parse_dir_name(dir_name: &str) -> Option<(String, String)> {
    // Find the last "--" which separates name from version
    let last_sep = dir_name.rfind("--")?;
    let name_part = &dir_name[..last_sep];
    let version = &dir_name[last_sep + 2..];

    // Convert "my-org--sentiment-analysis" back to "my-org/sentiment-analysis"
    // Only the first "--" after org should become "/"
    let name = if let Some(first_sep) = name_part.find("--") {
      format!(
        "{}/{}",
        &name_part[..first_sep],
        &name_part[first_sep + 2..]
      )
    } else {
      name_part.to_string()
    };

    Some((name, version.to_string()))
  }

  /// Read the manifest from a component directory.
  async fn read_manifest(&self, component_dir: &Path) -> Result<ComponentManifest, RegistryError> {
    let manifest_path = component_dir.join("manifest.json");
    let content = fs::read_to_string(&manifest_path).await?;
    let manifest: ComponentManifest = serde_json::from_str(&content)?;
    Ok(manifest)
  }

  /// Build an InstalledComponent from a component directory.
  async fn load_component(
    &self,
    component_dir: PathBuf,
  ) -> Result<InstalledComponent, RegistryError> {
    let manifest = self.read_manifest(&component_dir).await?;
    let wasm_path = component_dir.join("component.wasm");

    Ok(InstalledComponent {
      manifest,
      wasm_path,
      component_dir,
    })
  }
}

#[async_trait]
impl ComponentRegistry for FsComponentRegistry {
  async fn get(
    &self,
    name: &str,
    version: Option<&str>,
  ) -> Result<Option<InstalledComponent>, RegistryError> {
    let mut entries = fs::read_dir(&self.root).await?;
    let mut matching_components: Vec<InstalledComponent> = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
      let path = entry.path();
      if !path.is_dir() {
        continue;
      }

      let dir_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => continue,
      };

      if let Some((parsed_name, parsed_version)) = Self::parse_dir_name(dir_name)
        && parsed_name == name
      {
        if let Some(v) = version {
          if parsed_version == v {
            return Ok(Some(self.load_component(path).await?));
          }
        } else {
          matching_components.push(self.load_component(path).await?);
        }
      }
    }

    if version.is_none() && !matching_components.is_empty() {
      // Return the "latest" version (simple string sort for now)
      matching_components.sort_by(|a, b| b.manifest.version.cmp(&a.manifest.version));
      return Ok(matching_components.into_iter().next());
    }

    Ok(None)
  }

  async fn install(&self, package_path: &Path) -> Result<InstalledComponent, RegistryError> {
    // Read manifest from the package
    let manifest = self.read_manifest(package_path).await?;
    let target_dir = self.root.join(manifest.dir_name());

    // Check if already exists
    if target_dir.exists() {
      return Err(RegistryError::AlreadyExists {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
      });
    }

    // Create target directory
    fs::create_dir_all(&target_dir).await?;

    // Copy all files from package to target
    let mut entries = fs::read_dir(package_path).await?;
    while let Some(entry) = entries.next_entry().await? {
      let src = entry.path();
      let file_name = match src.file_name() {
        Some(n) => n,
        None => continue,
      };
      let dest = target_dir.join(file_name);

      if src.is_dir() {
        copy_dir_recursive(&src, &dest).await?;
      } else {
        fs::copy(&src, &dest).await?;
      }
    }

    self.load_component(target_dir).await
  }

  async fn list(&self) -> Result<Vec<ComponentManifest>, RegistryError> {
    let mut manifests = Vec::new();

    if !self.root.exists() {
      return Ok(manifests);
    }

    let mut entries = fs::read_dir(&self.root).await?;
    while let Some(entry) = entries.next_entry().await? {
      let path = entry.path();
      if path.is_dir()
        && let Ok(manifest) = self.read_manifest(&path).await
      {
        manifests.push(manifest);
      }
    }

    Ok(manifests)
  }

  async fn remove(&self, name: &str, version: &str) -> Result<(), RegistryError> {
    let component = self.get(name, Some(version)).await?;

    match component {
      Some(c) => {
        fs::remove_dir_all(&c.component_dir).await?;
        Ok(())
      }
      None => Err(RegistryError::VersionNotFound {
        name: name.to_string(),
        version: version.to_string(),
      }),
    }
  }
}

/// Recursively copy a directory.
async fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), std::io::Error> {
  fs::create_dir_all(dest).await?;

  let mut entries = fs::read_dir(src).await?;
  while let Some(entry) = entries.next_entry().await? {
    let src_path = entry.path();
    let dest_path = dest.join(entry.file_name());

    if src_path.is_dir() {
      Box::pin(copy_dir_recursive(&src_path, &dest_path)).await?;
    } else {
      fs::copy(&src_path, &dest_path).await?;
    }
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_dir_name_with_org() {
    let result = FsComponentRegistry::parse_dir_name("my-org--sentiment-analysis--1.0.0");
    assert_eq!(
      result,
      Some(("my-org/sentiment-analysis".to_string(), "1.0.0".to_string()))
    );
  }

  #[test]
  fn test_parse_dir_name_without_org() {
    let result = FsComponentRegistry::parse_dir_name("simple-component--2.1.0");
    assert_eq!(
      result,
      Some(("simple-component".to_string(), "2.1.0".to_string()))
    );
  }

  #[test]
  fn test_parse_dir_name_invalid() {
    let result = FsComponentRegistry::parse_dir_name("no-version-here");
    assert_eq!(result, None);
  }
}
