use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Metadata describing an installed component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentManifest {
  /// Component name, e.g. "my-org/google-sheets"
  pub name: String,

  /// Semantic version, e.g. "1.0.0"
  pub version: String,

  /// Short description of what the component does
  pub description: String,

  /// SHA-256 digest of the wasm binary, e.g. "sha256:abc123..."
  pub digest: String,

  /// Capabilities required by this component
  #[serde(default)]
  pub capabilities: ComponentCapabilities,

  /// Tasks exported by this component
  #[serde(default)]
  pub tasks: HashMap<String, TaskExport>,
}

/// Capabilities required by a component.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentCapabilities {
  /// Allowed HTTP hosts (e.g., ["api.google.com", "*.googleapis.com"])
  #[serde(default)]
  pub allowed_hosts: Vec<String>,

  /// Allowed file system paths (e.g., ["/tmp", "/data"])
  #[serde(default)]
  pub allowed_paths: Vec<String>,
}

/// A task exported by a component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExport {
  /// Description of what this task does
  pub description: String,

  /// JSON Schema for task inputs
  pub schema: serde_json::Value,
}

impl ComponentManifest {
  /// Returns the directory name for this component: "name--version"
  /// with slashes replaced by double dashes.
  ///
  /// Example: "my-org/google-sheets" version "1.0.0"
  /// becomes "my-org--google-sheets--1.0.0"
  pub fn dir_name(&self) -> String {
    let sanitized_name = self.name.replace('/', "--");
    format!("{}--{}", sanitized_name, self.version)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_dir_name() {
    let manifest = ComponentManifest {
      name: "my-org/sentiment-analysis".to_string(),
      version: "1.0.0".to_string(),
      description: "Analyzes sentiment".to_string(),
      digest: "sha256:abc123".to_string(),
      capabilities: ComponentCapabilities::default(),
      tasks: HashMap::new(),
    };

    assert_eq!(manifest.dir_name(), "my-org--sentiment-analysis--1.0.0");
  }

  #[test]
  fn test_dir_name_no_org() {
    let manifest = ComponentManifest {
      name: "simple-component".to_string(),
      version: "2.1.0".to_string(),
      description: "A simple component".to_string(),
      digest: "sha256:def456".to_string(),
      capabilities: ComponentCapabilities::default(),
      tasks: HashMap::new(),
    };

    assert_eq!(manifest.dir_name(), "simple-component--2.1.0");
  }

  #[test]
  fn test_manifest_with_tasks() {
    let mut tasks = HashMap::new();
    tasks.insert(
      "write-row".to_string(),
      TaskExport {
        description: "Write a row to a sheet".to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "spreadsheet_id": { "type": "string" },
                "values": { "type": "array" }
            }
        }),
      },
    );

    let manifest = ComponentManifest {
      name: "my-org/google-sheets".to_string(),
      version: "1.0.0".to_string(),
      description: "Google Sheets integration".to_string(),
      digest: "sha256:abc123".to_string(),
      capabilities: ComponentCapabilities {
        allowed_hosts: vec!["sheets.googleapis.com".to_string()],
        allowed_paths: vec![],
      },
      tasks,
    };

    assert_eq!(manifest.tasks.len(), 1);
    assert!(manifest.tasks.contains_key("write-row"));
  }
}
