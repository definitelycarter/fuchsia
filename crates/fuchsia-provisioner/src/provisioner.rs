use std::collections::BTreeMap;
use std::sync::Arc;

use fuchsia_actor::{ActorCapabilities, ActorConfig, ActorId};
use fuchsia_engine::Engine;
use fuchsia_workflow::store::{NodeDefinition, Workflow, WorkflowId};

use crate::error::ProvisionerError;

/// A workflow translated into engine terms: nodes as (address, actor type,
/// config), edges as (from, to). The node's `env` + opaque `settings` are
/// carried through as the actor's [`ActorConfig`]; the actor itself owns the
/// schema for `settings`.
#[derive(Debug, PartialEq)]
struct GraphSpec {
  group: String,
  nodes: Vec<(ActorId, String, ActorConfig)>,
  edges: Vec<(ActorId, ActorId)>,
}

/// Translate a stored workflow into a graph spec. Pure — no engine, no I/O —
/// so the translation is testable on its own. The workflow's id is the group,
/// which namespaces its node ids into global `ActorId`s.
fn plan(workflow: &Workflow) -> GraphSpec {
  let group = workflow.id.to_string();

  let nodes = workflow
    .nodes
    .iter()
    .map(|node| {
      // A cold, once-per-registration translation, so cloning the borrowed
      // definition's owned config into the spec is fine.
      let (type_name, config) = match &node.definition {
        NodeDefinition::Builtin(b) => (
          b.name.clone(),
          ActorConfig {
            env: b.env.clone(),
            settings: b.settings.clone(),
          },
        ),
        // ComponentConfig has no `env` field yet (provisional shape); pass an
        // empty environment until it grows one.
        NodeDefinition::Component(c) => (
          c.component.clone(),
          ActorConfig {
            env: BTreeMap::new(),
            settings: c.settings.clone(),
          },
        ),
      };
      (
        ActorId::scoped(group.as_str(), node.id.0.as_str()),
        type_name,
        config,
      )
    })
    .collect();

  let edges = workflow
    .edges
    .iter()
    .map(|edge| {
      (
        ActorId::scoped(group.as_str(), edge.from.0.as_str()),
        ActorId::scoped(group.as_str(), edge.to.0.as_str()),
      )
    })
    .collect();

  GraphSpec {
    group,
    nodes,
    edges,
  }
}

/// Turns workflow definitions into running engine graphs.
///
/// It holds a shared `Arc<Engine>` and owns the domain→graph translation; the
/// engine stays binding-agnostic.
pub struct Provisioner {
  engine: Arc<Engine>,
}

impl Provisioner {
  pub fn new(engine: Arc<Engine>) -> Self {
    Self { engine }
  }

  /// Provision a stored workflow into the engine as a grouped graph (group =
  /// the workflow's id). The actors are standing — one set per workflow
  /// definition, shared across runs; per-run state is keyed by run id, not by
  /// spinning up fresh actors.
  pub async fn register_workflow(&self, workflow: &Workflow) -> Result<(), ProvisionerError> {
    self.apply(plan(workflow)).await
  }

  /// Tear a workflow's graph down (stops its actors, drops its edges).
  pub async fn unregister_workflow(&self, id: &WorkflowId) -> Result<(), ProvisionerError> {
    self.engine.remove_graph(&id.to_string()).await?;
    Ok(())
  }

  async fn apply(&self, spec: GraphSpec) -> Result<(), ProvisionerError> {
    for (id, type_name, config) in spec.nodes {
      // Workflow nodes get no extra capabilities; the engine adds routing.
      self
        .engine
        .add_node(id, &type_name, &config, ActorCapabilities::new())
        .await?;
    }
    for (from, to) in spec.edges {
      self.engine.add_edge(from, to)?;
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use fuchsia_workflow::store::{BuiltinConfig, Edge, Node, NodeId, Workflow, WorkflowId};

  fn builtin_node(id: &str, name: &str) -> Node {
    Node {
      id: NodeId(id.to_owned()),
      definition: NodeDefinition::Builtin(BuiltinConfig {
        name: name.to_owned(),
        env: Default::default(),
        settings: Default::default(),
      }),
      trigger: None,
    }
  }

  #[test]
  fn plan_namespaces_nodes_and_edges_by_workflow_id() {
    let workflow = Workflow {
      id: WorkflowId::new(),
      name: "climate".to_owned(),
      nodes: vec![
        builtin_node("a", "passthrough"),
        builtin_node("b", "recorder"),
      ],
      edges: vec![Edge {
        from: NodeId("a".to_owned()),
        to: NodeId("b".to_owned()),
      }],
      created_at: 0,
      updated_at: 0,
    };
    let group = workflow.id.to_string();

    let spec = plan(&workflow);

    assert_eq!(spec.group, group);
    assert_eq!(
      spec.nodes,
      vec![
        (
          ActorId::scoped(group.as_str(), "a"),
          "passthrough".to_owned(),
          ActorConfig::default(),
        ),
        (
          ActorId::scoped(group.as_str(), "b"),
          "recorder".to_owned(),
          ActorConfig::default(),
        ),
      ]
    );
    assert_eq!(
      spec.edges,
      vec![(
        ActorId::scoped(group.as_str(), "a"),
        ActorId::scoped(group.as_str(), "b"),
      )]
    );
  }
}
