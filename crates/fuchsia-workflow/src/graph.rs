use std::collections::{HashMap, HashSet};

use crate::Node;

/// Graph structure for traversal and analysis.
#[derive(Debug, Clone)]
pub struct Graph {
  /// Adjacency list: node_id -> list of downstream node_ids.
  adjacency: HashMap<String, Vec<String>>,
  /// Reverse adjacency: node_id -> list of upstream node_ids.
  reverse_adjacency: HashMap<String, Vec<String>>,
  /// Nodes with no incoming edges.
  entry_points: Vec<String>,
  /// Nodes with multiple incoming edges (join points).
  join_points: HashSet<String>,
}

impl Graph {
  /// Build a graph from nodes and edges.
  pub fn new(nodes: &HashMap<String, Node>, edges: &[(String, String)]) -> Self {
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    let mut reverse_adjacency: HashMap<String, Vec<String>> = HashMap::new();

    // Initialize all nodes
    for node_id in nodes.keys() {
      adjacency.entry(node_id.clone()).or_default();
      reverse_adjacency.entry(node_id.clone()).or_default();
    }

    // Build adjacency lists
    for (from, to) in edges {
      adjacency.entry(from.clone()).or_default().push(to.clone());
      reverse_adjacency
        .entry(to.clone())
        .or_default()
        .push(from.clone());
    }

    // Find entry points (no incoming edges)
    let entry_points: Vec<String> = nodes
      .keys()
      .filter(|id| reverse_adjacency.get(*id).is_none_or(|v| v.is_empty()))
      .cloned()
      .collect();

    // Find join points (multiple incoming edges)
    let join_points: HashSet<String> = reverse_adjacency
      .iter()
      .filter(|(_, incoming)| incoming.len() > 1)
      .map(|(id, _)| id.clone())
      .collect();

    Self {
      adjacency,
      reverse_adjacency,
      entry_points,
      join_points,
    }
  }

  /// Get entry points (nodes with no incoming edges).
  pub fn entry_points(&self) -> &[String] {
    &self.entry_points
  }

  /// Get downstream nodes for a given node.
  pub fn downstream(&self, node_id: &str) -> &[String] {
    self
      .adjacency
      .get(node_id)
      .map(|v| v.as_slice())
      .unwrap_or(&[])
  }

  /// Get upstream nodes for a given node.
  pub fn upstream(&self, node_id: &str) -> &[String] {
    self
      .reverse_adjacency
      .get(node_id)
      .map(|v| v.as_slice())
      .unwrap_or(&[])
  }

  /// Check if a node is a join point (has multiple incoming edges).
  pub fn is_join_point(&self, node_id: &str) -> bool {
    self.join_points.contains(node_id)
  }

  /// Get all join points.
  pub fn join_points(&self) -> &HashSet<String> {
    &self.join_points
  }
}
