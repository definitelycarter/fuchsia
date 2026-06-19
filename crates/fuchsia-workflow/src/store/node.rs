use std::collections::BTreeMap;

use bson::Document;
use serde::{Deserialize, Serialize};

/// Author-assigned identifier for a node, unique within its workflow. Edges
/// reference nodes by this id, so it must be stable and meaningful to whoever
/// authors the workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

/// A node in the workflow graph: a stable identity plus what it does, and
/// optionally what *fires* it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
  pub id: NodeId,
  pub definition: NodeDefinition,
  /// What feeds this node from outside the graph. A node with a trigger is an
  /// *entry* — the host pushes into it when the source fires. Host-facing
  /// routing metadata; sits beside `definition` so the latter stays purely
  /// engine-facing (the engine never sees a trigger). `None` = an interior
  /// node, fed only by edges.
  #[serde(default)]
  pub trigger: Option<Trigger>,
}

/// A node's external input source — the dual of `emit` being its output.
///
/// Adjacently tagged (`{ "on": "...", "config": { ... } }`) so more sources
/// (schedules, webhooks) can be added without breaking stored workflows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "on", content = "config", rename_all = "snake_case")]
pub enum Trigger {
  /// Fire when the named entity's state changes. Referenced by id string, so a
  /// workflow definition doesn't depend on the entity crate.
  EntityChanged { entity: String },
}

/// What a node does, together with the configuration that goes with it.
///
/// Configuration is meaningless outside the context of a type, so the two
/// travel as a tagged union — the compiler refuses to pair one variant's data
/// with another's. Serializes adjacently as
/// `{ "type": "...", "configuration": { ... } }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "configuration", rename_all = "snake_case")]
pub enum NodeDefinition {
  /// A native builtin node, resolved by its registered type name. Behavior is
  /// compiled into the host (no sandbox) — passthrough, the conditioning
  /// operators, control flow.
  Builtin(BuiltinConfig),
  /// Behavior supplied by a registered actor-component (WASM or Lua). The
  /// component declares its own config schema, so its dynamic settings are an
  /// opaque document validated at runtime, not against a compile-time type.
  Component(ComponentConfig),
}

/// Config for a native builtin node: which registered type to instantiate,
/// the curated environment the host exposes to it, plus opaque per-operator
/// settings the host passes through untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltinConfig {
  pub name: String,
  /// Host-curated environment for this node — a sibling of `settings`, not
  /// flattened into it, so a typo'd key fails rather than silently becoming
  /// opaque config.
  #[serde(default)]
  pub env: BTreeMap<String, String>,
  #[serde(default)]
  pub settings: Document,
}

/// Which sandboxed runtime backs a component. Named here only enough to route
/// to the right registry; the registry owns the actual backing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
  Wasm,
  Lua,
}

/// Reference to a registered component plus its dynamic configuration.
///
/// Provisional shape — the concrete fields will firm up later. `settings` is
/// the component-specific config, validated against the component's declared
/// schema at runtime (the same way an entity action validates its arguments).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentConfig {
  pub runtime: Runtime,
  /// Identity of the registered component to instantiate.
  pub component: String,
  #[serde(default)]
  pub settings: Document,
}

/// A directed edge between two nodes. Carries only the connection for now —
/// conditions and output ports come later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
  pub from: NodeId,
  pub to: NodeId,
}
