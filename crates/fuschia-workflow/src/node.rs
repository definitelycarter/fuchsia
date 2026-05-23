use std::collections::HashMap;

use fuschia_config::{ExecutionMode, InputValue, JoinStrategy, LoopFailureMode, RuntimeType};
use serde::{Deserialize, Serialize};

/// A resolved node in a locked workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
  pub node_id: String,
  pub node_type: NodeType,
  pub inputs: HashMap<String, InputValue>,
  pub timeout_ms: Option<u64>,
  pub max_retry_attempts: Option<u32>,
  pub fail_workflow: bool,
}

/// The type of a resolved node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeType {
  /// A component task node.
  Component(LockedComponent),
  /// A join node that merges multiple branches.
  Join { strategy: JoinStrategy },
  /// A loop node with a nested workflow.
  Loop(LockedLoop),
}

/// A locked task component that has been resolved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockedComponent {
  /// Component name (e.g., "my-org/sentiment-analysis")
  pub name: String,

  /// Component version (e.g., "1.0.0")
  pub version: String,

  /// SHA-256 digest of the wasm binary (for verification)
  pub digest: String,

  /// Name of the task export within the component (e.g., "write-row")
  pub task_name: String,

  /// JSON Schema for the task's input (copied from manifest at lock time)
  pub input_schema: serde_json::Value,

  /// The runtime backend this component targets.
  #[serde(default)]
  pub runtime_type: RuntimeType,
}

/// A loop node with its nested workflow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockedLoop {
  pub execution_mode: ExecutionMode,
  pub concurrency: Option<u32>,
  pub failure_mode: LoopFailureMode,
  /// The nested workflow inside the loop.
  pub workflow: Box<crate::Workflow>,
}
