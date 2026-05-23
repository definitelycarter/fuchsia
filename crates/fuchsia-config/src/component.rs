use serde::{Deserialize, Serialize};

/// Reference to a component in the registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentRef {
  /// Component name, e.g. "my-org/sentiment-analysis"
  pub name: String,

  /// Optional version constraint, e.g. "1.0.0"
  /// If not specified, uses the latest installed version.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub version: Option<String>,
}
