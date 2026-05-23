use serde::{Deserialize, Serialize};

use crate::edge::Edge;
use crate::enums::RetryBackoff;
use crate::node::NodeDef;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDef {
  pub workflow_id: String,
  pub name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub timeout_ms: Option<u64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub max_retry_attempts: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub retry_backoff: Option<RetryBackoff>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub retry_initial_delay_ms: Option<u64>,
  pub nodes: Vec<NodeDef>,
  pub edges: Vec<Edge>,
}
