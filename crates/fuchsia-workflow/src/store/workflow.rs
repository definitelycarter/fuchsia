use std::fmt;

use bson::oid::ObjectId;
use serde::{Deserialize, Serialize};

use super::error::WorkflowError;
use super::node::{Edge, Node};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowId(pub(crate) ObjectId);

impl WorkflowId {
  /// Mint a fresh workflow id. The store also assigns one on `create`; this is
  /// for constructing workflows in tests or before persistence.
  pub fn new() -> Self {
    Self(ObjectId::new())
  }

  /// Parse an id from its hex string form (e.g. a URL path segment). The error
  /// is the parse message — callers (e.g. the HTTP layer) map it to a 400.
  pub fn parse(s: &str) -> Result<Self, String> {
    ObjectId::parse_str(s).map(Self).map_err(|e| e.to_string())
  }
}

impl fmt::Display for WorkflowId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.0.fmt(f)
  }
}

/// A workflow definition: a directed graph of nodes. Always durable — the
/// definition is config, so unlike an entity it carries no durability policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
  #[serde(rename = "_id")]
  pub id: WorkflowId,
  pub name: String,
  pub nodes: Vec<Node>,
  pub edges: Vec<Edge>,
  pub created_at: i64,
  pub updated_at: i64,
}

impl Workflow {
  /// Structural validation. A trigger node is an *entry* — fed by its trigger —
  /// so it must have no incoming edge; an internal edge into it would be a
  /// second, competing input source.
  pub fn validate(&self) -> Result<(), WorkflowError> {
    for node in &self.nodes {
      if node.trigger.is_some() && self.edges.iter().any(|edge| edge.to == node.id) {
        return Err(WorkflowError::Invalid(format!(
          "trigger node `{}` has an incoming edge; a trigger node must be a source",
          node.id.0
        )));
      }
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::super::node::{BuiltinConfig, NodeDefinition, NodeId, Trigger};
  use super::*;

  fn node(id: &str, trigger: Option<Trigger>) -> Node {
    Node {
      id: NodeId(id.to_owned()),
      definition: NodeDefinition::Builtin(BuiltinConfig {
        name: "passthrough".to_owned(),
        env: Default::default(),
        settings: Default::default(),
      }),
      trigger,
    }
  }

  fn workflow(nodes: Vec<Node>, edges: Vec<Edge>) -> Workflow {
    Workflow {
      id: WorkflowId::new(),
      name: "w".to_owned(),
      nodes,
      edges,
      created_at: 0,
      updated_at: 0,
    }
  }

  fn on_entity(id: &str) -> Trigger {
    Trigger::EntityChanged {
      entity: id.to_owned(),
    }
  }

  #[test]
  fn trigger_node_as_source_is_valid() {
    // a (trigger) → b
    let wf = workflow(
      vec![node("a", Some(on_entity("e"))), node("b", None)],
      vec![Edge {
        from: NodeId("a".to_owned()),
        to: NodeId("b".to_owned()),
      }],
    );
    assert!(wf.validate().is_ok());
  }

  #[test]
  fn trigger_node_with_incoming_edge_is_invalid() {
    // a → b (trigger) — an internal edge feeds a trigger node
    let wf = workflow(
      vec![node("a", None), node("b", Some(on_entity("e")))],
      vec![Edge {
        from: NodeId("a".to_owned()),
        to: NodeId("b".to_owned()),
      }],
    );
    assert!(matches!(wf.validate(), Err(WorkflowError::Invalid(_))));
  }
}

/// Input for creating a new workflow. The store assigns the id and timestamps.
/// `Deserialize` so it's the body of `POST /workflows`; `nodes`/`edges` default
/// to empty, so a minimal create is `{ "name" }`.
#[derive(Debug, Deserialize)]
pub struct NewWorkflow {
  pub name: String,
  #[serde(default)]
  pub nodes: Vec<Node>,
  #[serde(default)]
  pub edges: Vec<Edge>,
}
