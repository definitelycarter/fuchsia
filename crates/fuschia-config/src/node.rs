use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::component::ComponentRef;
use crate::edge::Edge;
use crate::enums::{ExecutionMode, JoinStrategy, LoopFailureMode};
use crate::input::InputValue;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeDef {
  pub node_id: String,
  #[serde(flatten)]
  pub node_type: NodeType,
  #[serde(default)]
  pub inputs: HashMap<String, InputValue>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub timeout_ms: Option<u64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub max_retry_attempts: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub fail_workflow: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeType {
  #[serde(rename = "component")]
  Component {
    #[serde(flatten)]
    component: ComponentRef,
    /// Name of the task export within the component (e.g., "write-row")
    task_name: String,
  },
  Join {
    join_strategy: JoinStrategy,
  },
  Loop {
    execution_mode: ExecutionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    concurrency: Option<u32>,
    failure_mode: LoopFailureMode,
    nodes: Vec<NodeDef>,
    edges: Vec<Edge>,
  },
}
