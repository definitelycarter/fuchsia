use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::graph::Graph;
use crate::node::Node;

/// A locked workflow ready for execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
  pub workflow_id: String,
  pub name: String,
  pub nodes: HashMap<String, Node>,
  pub edges: Vec<(String, String)>,
  pub timeout_ms: Option<u64>,
  pub max_retry_attempts: Option<u32>,
}

impl Workflow {
  /// Build the graph structure for traversal.
  pub fn graph(&self) -> Graph {
    Graph::new(&self.nodes, &self.edges)
  }

  /// Get a node by ID.
  pub fn get_node(&self, node_id: &str) -> Option<&Node> {
    self.nodes.get(node_id)
  }
}
