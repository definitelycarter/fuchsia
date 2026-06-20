use std::fmt;

use bson::oid::ObjectId;
use serde::{Deserialize, Serialize};

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
///
/// The graph carries no notion of what *fires* it: triggering is a consumer
/// concern (detect an event, then `engine.push` a message into the chosen
/// node), so the engine and this definition stay invocation-agnostic.
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
