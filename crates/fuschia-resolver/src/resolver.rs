use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;

use fuschia_component_registry::{ComponentRegistry, InstalledComponent};
use fuschia_config::{NodeDef, NodeType as ConfigNodeType, WorkflowDef};
use fuschia_workflow::{LockedComponent, LockedLoop, Node, NodeType, Workflow};

use crate::error::ResolveError;

/// Resolver transforms a WorkflowDef into a locked Workflow.
#[async_trait]
pub trait Resolver: Send + Sync {
  /// Resolve a workflow definition into a locked workflow.
  ///
  /// This process:
  /// 1. Validates the graph structure (no cycles, valid edges)
  /// 2. Resolves component references to installed components
  /// 3. Builds the locked workflow with content digests
  async fn resolve(&self, def: WorkflowDef) -> Result<Workflow, ResolveError>;
}

/// Standard resolver implementation that uses a component registry.
pub struct StandardResolver<R: ComponentRegistry> {
  registry: R,
}

impl<R: ComponentRegistry> StandardResolver<R> {
  /// Create a new resolver with the given component registry.
  pub fn new(registry: R) -> Self {
    Self { registry }
  }

  /// Validate that all edges reference existing nodes.
  fn validate_edges(
    &self,
    node_ids: &HashSet<String>,
    edges: &[(String, String)],
  ) -> Result<(), ResolveError> {
    for (from, to) in edges {
      if !node_ids.contains(from) {
        return Err(ResolveError::InvalidEdge {
          node_id: from.clone(),
        });
      }
      if !node_ids.contains(to) {
        return Err(ResolveError::InvalidEdge {
          node_id: to.clone(),
        });
      }
    }
    Ok(())
  }

  /// Check for cycles using DFS.
  fn detect_cycle(
    &self,
    node_ids: &HashSet<String>,
    edges: &[(String, String)],
  ) -> Result<(), ResolveError> {
    // Build adjacency list
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for node_id in node_ids {
      adjacency.insert(node_id.as_str(), Vec::new());
    }
    for (from, to) in edges {
      if let Some(neighbors) = adjacency.get_mut(from.as_str()) {
        neighbors.push(to.as_str());
      }
    }

    // DFS with coloring: 0 = white (unvisited), 1 = gray (in progress), 2 = black (done)
    let mut color: HashMap<&str, u8> = node_ids.iter().map(|id| (id.as_str(), 0u8)).collect();

    fn dfs<'a>(
      node: &'a str,
      adjacency: &HashMap<&str, Vec<&'a str>>,
      color: &mut HashMap<&'a str, u8>,
    ) -> bool {
      color.insert(node, 1); // Mark as in progress

      if let Some(neighbors) = adjacency.get(node) {
        for &neighbor in neighbors {
          match color.get(neighbor) {
            Some(1) => return true, // Back edge = cycle
            Some(0) => {
              if dfs(neighbor, adjacency, color) {
                return true;
              }
            }
            _ => {}
          }
        }
      }

      color.insert(node, 2); // Mark as done
      false
    }

    for node_id in node_ids {
      if color.get(node_id.as_str()) == Some(&0) && dfs(node_id.as_str(), &adjacency, &mut color) {
        return Err(ResolveError::CycleDetected);
      }
    }

    Ok(())
  }

  /// Resolve a single node definition into a locked node.
  fn resolve_node<'a>(
    &'a self,
    node_def: NodeDef,
  ) -> Pin<Box<dyn Future<Output = Result<Node, ResolveError>> + Send + 'a>> {
    Box::pin(async move {
      let node_type =
        match node_def.node_type {
          ConfigNodeType::Component {
            component,
            task_name,
          } => {
            let installed = self
              .lookup_component(&component.name, component.version.as_deref())
              .await?;

            // Look up the task in the manifest to get its schema
            let task_export = installed.manifest.tasks.get(&task_name).ok_or_else(|| {
              ResolveError::TaskNotFound {
                component: installed.manifest.name.clone(),
                task_name: task_name.clone(),
              }
            })?;

            NodeType::Component(LockedComponent {
              name: installed.manifest.name,
              version: installed.manifest.version,
              digest: installed.manifest.digest,
              task_name,
              input_schema: task_export.schema.clone(),
              runtime_type: Default::default(),
            })
          }
          ConfigNodeType::Join { join_strategy } => NodeType::Join {
            strategy: join_strategy,
          },
          ConfigNodeType::Loop {
            execution_mode,
            concurrency,
            failure_mode,
            nodes,
            edges,
          } => {
            // Recursively resolve the nested workflow
            let nested_workflow = self
              .resolve_inner(
                format!("{}_loop", node_def.node_id),
                format!("{} (loop body)", node_def.node_id),
                nodes,
                edges,
                None,
                None,
              )
              .await?;

            NodeType::Loop(LockedLoop {
              execution_mode,
              concurrency,
              failure_mode,
              workflow: Box::new(nested_workflow),
            })
          }
        };

      Ok(Node {
        node_id: node_def.node_id,
        node_type,
        inputs: node_def.inputs,
        timeout_ms: node_def.timeout_ms,
        max_retry_attempts: node_def.max_retry_attempts,
        fail_workflow: node_def.fail_workflow.unwrap_or(false),
      })
    })
  }

  /// Look up a component in the registry.
  async fn lookup_component(
    &self,
    name: &str,
    version: Option<&str>,
  ) -> Result<InstalledComponent, ResolveError> {
    match self.registry.get(name, version).await? {
      Some(component) => Ok(component),
      None => match version {
        Some(v) => Err(ResolveError::ComponentVersionNotFound {
          name: name.to_string(),
          version: v.to_string(),
        }),
        None => Err(ResolveError::ComponentNotFound {
          name: name.to_string(),
        }),
      },
    }
  }

  /// Inner resolve function for both top-level and nested workflows.
  fn resolve_inner<'a>(
    &'a self,
    workflow_id: String,
    name: String,
    nodes: Vec<NodeDef>,
    edges: Vec<fuschia_config::Edge>,
    timeout_ms: Option<u64>,
    max_retry_attempts: Option<u32>,
  ) -> Pin<Box<dyn Future<Output = Result<Workflow, ResolveError>> + Send + 'a>> {
    Box::pin(async move {
      // Collect node IDs and check for duplicates
      let mut node_ids = HashSet::new();
      for node in &nodes {
        if !node_ids.insert(node.node_id.clone()) {
          return Err(ResolveError::DuplicateNodeId {
            node_id: node.node_id.clone(),
          });
        }
      }

      // Convert edges to tuple format
      let edge_tuples: Vec<(String, String)> = edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();

      // Validate edges
      self.validate_edges(&node_ids, &edge_tuples)?;

      // Check for cycles
      self.detect_cycle(&node_ids, &edge_tuples)?;

      // Resolve each node
      let mut resolved_nodes = HashMap::new();
      for node_def in nodes {
        let node_id = node_def.node_id.clone();
        let node = self.resolve_node(node_def).await?;
        resolved_nodes.insert(node_id, node);
      }

      let workflow = Workflow {
        workflow_id,
        name,
        nodes: resolved_nodes,
        edges: edge_tuples,
        timeout_ms,
        max_retry_attempts,
      };

      // Validate entry points exist
      let graph = workflow.graph();
      if graph.entry_points().is_empty() {
        return Err(ResolveError::NoEntryPoints);
      }

      Ok(workflow)
    })
  }
}

#[async_trait]
impl<R: ComponentRegistry> Resolver for StandardResolver<R> {
  async fn resolve(&self, def: WorkflowDef) -> Result<Workflow, ResolveError> {
    self
      .resolve_inner(
        def.workflow_id,
        def.name,
        def.nodes,
        def.edges,
        def.timeout_ms,
        def.max_retry_attempts,
      )
      .await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use fuschia_component_registry::{ComponentManifest, RegistryError};
  use fuschia_config::{ComponentRef, Edge, JoinStrategy};
  use std::path::{Path, PathBuf};
  use std::sync::Mutex;

  /// Mock component registry for testing.
  struct MockRegistry {
    components: Mutex<HashMap<String, InstalledComponent>>,
  }

  impl MockRegistry {
    fn new() -> Self {
      Self {
        components: Mutex::new(HashMap::new()),
      }
    }

    fn add_component(&self, name: &str, version: &str, digest: &str) {
      self.add_component_with_tasks(name, version, digest, vec!["default"]);
    }

    fn add_component_with_tasks(
      &self,
      name: &str,
      version: &str,
      digest: &str,
      task_names: Vec<&str>,
    ) {
      use fuschia_component_registry::TaskExport;

      let mut tasks = HashMap::new();
      for task_name in task_names {
        tasks.insert(
          task_name.to_string(),
          TaskExport {
            description: format!("Task {}", task_name),
            schema: serde_json::json!({
              "type": "object",
              "properties": {}
            }),
          },
        );
      }

      let manifest = ComponentManifest {
        name: name.to_string(),
        version: version.to_string(),
        description: "Test component".to_string(),
        digest: digest.to_string(),
        capabilities: Default::default(),
        tasks,
      };
      let installed = InstalledComponent {
        manifest,
        wasm_path: PathBuf::from("/tmp/test.wasm"),
        component_dir: PathBuf::from("/tmp"),
      };
      let key = format!("{}@{}", name, version);
      self.components.lock().unwrap().insert(key, installed);
    }
  }

  #[async_trait]
  impl ComponentRegistry for MockRegistry {
    async fn get(
      &self,
      name: &str,
      version: Option<&str>,
    ) -> Result<Option<InstalledComponent>, RegistryError> {
      let components = self.components.lock().unwrap();

      if let Some(v) = version {
        let key = format!("{}@{}", name, v);
        return Ok(components.get(&key).cloned());
      }

      // Find latest version (simple: just find any matching name)
      for (key, component) in components.iter() {
        if key.starts_with(&format!("{}@", name)) {
          return Ok(Some(component.clone()));
        }
      }

      Ok(None)
    }

    async fn install(&self, _package_path: &Path) -> Result<InstalledComponent, RegistryError> {
      unimplemented!("not needed for tests")
    }

    async fn list(&self) -> Result<Vec<ComponentManifest>, RegistryError> {
      let components = self.components.lock().unwrap();
      Ok(components.values().map(|c| c.manifest.clone()).collect())
    }

    async fn remove(&self, _name: &str, _version: &str) -> Result<(), RegistryError> {
      unimplemented!("not needed for tests")
    }
  }

  fn make_component_node(id: &str, component_name: &str) -> NodeDef {
    make_component_node_with_task(id, component_name, "default")
  }

  fn make_component_node_with_task(id: &str, component_name: &str, task_name: &str) -> NodeDef {
    NodeDef {
      node_id: id.to_string(),
      node_type: ConfigNodeType::Component {
        component: ComponentRef {
          name: component_name.to_string(),
          version: Some("1.0.0".to_string()),
        },
        task_name: task_name.to_string(),
      },
      inputs: HashMap::new(),
      timeout_ms: None,
      max_retry_attempts: None,
      fail_workflow: None,
    }
  }

  fn make_join_node(id: &str) -> NodeDef {
    NodeDef {
      node_id: id.to_string(),
      node_type: ConfigNodeType::Join {
        join_strategy: JoinStrategy::All,
      },
      inputs: HashMap::new(),
      timeout_ms: None,
      max_retry_attempts: None,
      fail_workflow: None,
    }
  }

  #[tokio::test]
  async fn test_resolve_simple_workflow() {
    let registry = MockRegistry::new();
    registry.add_component("my-org/processor", "1.0.0", "sha256:abc123");

    let resolver = StandardResolver::new(registry);

    let def = WorkflowDef {
      workflow_id: "test-workflow".to_string(),
      name: "Test Workflow".to_string(),
      nodes: vec![make_component_node("step1", "my-org/processor")],
      edges: vec![],
      timeout_ms: None,
      max_retry_attempts: None,
      retry_backoff: None,
      retry_initial_delay_ms: None,
    };

    let workflow = resolver.resolve(def).await.unwrap();

    assert_eq!(workflow.workflow_id, "test-workflow");
    assert_eq!(workflow.nodes.len(), 1);

    let node = workflow.get_node("step1").unwrap();
    match &node.node_type {
      NodeType::Component(locked) => {
        assert_eq!(locked.name, "my-org/processor");
        assert_eq!(locked.version, "1.0.0");
        assert_eq!(locked.digest, "sha256:abc123");
        assert_eq!(locked.task_name, "default");
        assert!(locked.input_schema.is_object());
      }
      _ => panic!("expected component node"),
    }
  }

  #[tokio::test]
  async fn test_resolve_workflow_with_edges() {
    let registry = MockRegistry::new();
    registry.add_component("my-org/step1", "1.0.0", "sha256:aaa");
    registry.add_component("my-org/step2", "1.0.0", "sha256:bbb");

    let resolver = StandardResolver::new(registry);

    let def = WorkflowDef {
      workflow_id: "test".to_string(),
      name: "Test".to_string(),
      nodes: vec![
        make_component_node("a", "my-org/step1"),
        make_component_node("b", "my-org/step2"),
      ],
      edges: vec![Edge {
        from: "a".to_string(),
        to: "b".to_string(),
      }],
      timeout_ms: None,
      max_retry_attempts: None,
      retry_backoff: None,
      retry_initial_delay_ms: None,
    };

    let workflow = resolver.resolve(def).await.unwrap();

    assert_eq!(workflow.nodes.len(), 2);
    assert_eq!(workflow.edges.len(), 1);
    assert_eq!(workflow.edges[0], ("a".to_string(), "b".to_string()));

    let graph = workflow.graph();
    assert_eq!(graph.entry_points(), &["a".to_string()]);
  }

  #[tokio::test]
  async fn test_resolve_fails_on_missing_component() {
    let registry = MockRegistry::new();
    // Don't add any components

    let resolver = StandardResolver::new(registry);

    let def = WorkflowDef {
      workflow_id: "test".to_string(),
      name: "Test".to_string(),
      nodes: vec![make_component_node("step1", "missing/component")],
      edges: vec![],
      timeout_ms: None,
      max_retry_attempts: None,
      retry_backoff: None,
      retry_initial_delay_ms: None,
    };

    let result = resolver.resolve(def).await;
    // Since make_component_node specifies a version, we get ComponentVersionNotFound
    assert!(matches!(
      result,
      Err(ResolveError::ComponentVersionNotFound { .. })
    ));
  }

  #[tokio::test]
  async fn test_resolve_fails_on_invalid_edge() {
    let registry = MockRegistry::new();
    registry.add_component("my-org/processor", "1.0.0", "sha256:abc");

    let resolver = StandardResolver::new(registry);

    let def = WorkflowDef {
      workflow_id: "test".to_string(),
      name: "Test".to_string(),
      nodes: vec![make_component_node("step1", "my-org/processor")],
      edges: vec![Edge {
        from: "step1".to_string(),
        to: "nonexistent".to_string(),
      }],
      timeout_ms: None,
      max_retry_attempts: None,
      retry_backoff: None,
      retry_initial_delay_ms: None,
    };

    let result = resolver.resolve(def).await;
    assert!(matches!(result, Err(ResolveError::InvalidEdge { .. })));
  }

  #[tokio::test]
  async fn test_resolve_fails_on_cycle() {
    let registry = MockRegistry::new();
    registry.add_component("my-org/a", "1.0.0", "sha256:aaa");
    registry.add_component("my-org/b", "1.0.0", "sha256:bbb");

    let resolver = StandardResolver::new(registry);

    let def = WorkflowDef {
      workflow_id: "test".to_string(),
      name: "Test".to_string(),
      nodes: vec![
        make_component_node("a", "my-org/a"),
        make_component_node("b", "my-org/b"),
      ],
      edges: vec![
        Edge {
          from: "a".to_string(),
          to: "b".to_string(),
        },
        Edge {
          from: "b".to_string(),
          to: "a".to_string(),
        },
      ],
      timeout_ms: None,
      max_retry_attempts: None,
      retry_backoff: None,
      retry_initial_delay_ms: None,
    };

    let result = resolver.resolve(def).await;
    assert!(matches!(result, Err(ResolveError::CycleDetected)));
  }

  #[tokio::test]
  async fn test_resolve_fails_on_duplicate_node_id() {
    let registry = MockRegistry::new();
    registry.add_component("my-org/processor", "1.0.0", "sha256:abc");

    let resolver = StandardResolver::new(registry);

    let def = WorkflowDef {
      workflow_id: "test".to_string(),
      name: "Test".to_string(),
      nodes: vec![
        make_component_node("step1", "my-org/processor"),
        make_component_node("step1", "my-org/processor"), // Duplicate!
      ],
      edges: vec![],
      timeout_ms: None,
      max_retry_attempts: None,
      retry_backoff: None,
      retry_initial_delay_ms: None,
    };

    let result = resolver.resolve(def).await;
    assert!(matches!(result, Err(ResolveError::DuplicateNodeId { .. })));
  }

  #[tokio::test]
  async fn test_resolve_join_node() {
    let registry = MockRegistry::new();
    registry.add_component("my-org/a", "1.0.0", "sha256:aaa");
    registry.add_component("my-org/b", "1.0.0", "sha256:bbb");

    let resolver = StandardResolver::new(registry);

    let def = WorkflowDef {
      workflow_id: "test".to_string(),
      name: "Test".to_string(),
      nodes: vec![
        make_component_node("a", "my-org/a"),
        make_component_node("b", "my-org/b"),
        make_join_node("join"),
      ],
      edges: vec![
        Edge {
          from: "a".to_string(),
          to: "join".to_string(),
        },
        Edge {
          from: "b".to_string(),
          to: "join".to_string(),
        },
      ],
      timeout_ms: None,
      max_retry_attempts: None,
      retry_backoff: None,
      retry_initial_delay_ms: None,
    };

    let workflow = resolver.resolve(def).await.unwrap();

    let join_node = workflow.get_node("join").unwrap();
    assert!(matches!(join_node.node_type, NodeType::Join { .. }));

    let graph = workflow.graph();
    assert!(graph.is_join_point("join"));
  }
}
