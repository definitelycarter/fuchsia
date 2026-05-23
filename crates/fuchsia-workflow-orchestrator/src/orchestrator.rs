//! Workflow orchestrator.
//!
//! The [`Orchestrator`] handles DAG traversal, wave-based scheduling,
//! input resolution, and delegates all component execution to the
//! [`RuntimeRegistry`]. Workflows are invoked manually with a payload
//! that is fed to graph entry points.

use std::collections::HashMap;
use std::sync::Arc;

use fuchsia_component_registry::ComponentRegistry;
use fuchsia_config::RuntimeType as ConfigRuntimeType;
use fuchsia_task_runtime::{RuntimeRegistry, RuntimeType, TaskInput as RuntimeTaskInput};
use fuchsia_workflow::{NodeType, Workflow};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::capabilities::CapabilitiesFactory;
use crate::error::OrchestratorError;
use crate::input::{coerce_inputs, extract_schema_types, resolve_inputs};
use crate::result::{InvokeResult, NodeResult};

/// Handle for a spawned node task.
type NodeHandle = tokio::task::JoinHandle<Result<NodeResult, OrchestratorError>>;

/// Configuration for the orchestrator.
#[derive(Default)]
pub struct OrchestratorConfig {
    // Future: default_timeout_ms, max_concurrency, etc.
}

/// The workflow orchestrator.
///
/// Handles DAG traversal, wave-based scheduling, input resolution, and
/// delegates component execution to the `RuntimeRegistry`.
pub struct Orchestrator {
    workflow: Workflow,
    registry: Arc<RuntimeRegistry>,
    component_registry: Arc<dyn ComponentRegistry>,
    #[allow(dead_code)]
    config: OrchestratorConfig,
}

impl Orchestrator {
    /// Create a new orchestrator for the given workflow.
    pub fn new(
        workflow: Workflow,
        registry: Arc<RuntimeRegistry>,
        component_registry: Arc<dyn ComponentRegistry>,
        config: OrchestratorConfig,
    ) -> Result<Self, OrchestratorError> {
        Ok(Self {
            workflow,
            registry,
            component_registry,
            config,
        })
    }

    /// Execute the workflow with the given payload.
    ///
    /// The payload is fed to graph entry points (nodes with no incoming edges)
    /// as if it were the output of a single upstream node.
    #[instrument(
        name = "orchestrator_invoke",
        skip(self, payload, cancel),
        fields(workflow_id = %self.workflow.workflow_id)
    )]
    pub async fn invoke(
        &self,
        payload: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<InvokeResult, OrchestratorError> {
        let execution_id = uuid::Uuid::new_v4().to_string();

        info!(
            execution_id = %execution_id,
            workflow_id = %self.workflow.workflow_id,
            payload = %payload,
            "workflow_started"
        );

        self.validate_workflow()?;

        let capabilities_factory = CapabilitiesFactory::new();
        let mut completed: HashMap<String, NodeResult> = HashMap::new();

        let result = self
            .run_execution_loop(
                &mut completed,
                &payload,
                &execution_id,
                &cancel,
                &capabilities_factory,
            )
            .await;

        match &result {
            Ok(_) => {
                info!(execution_id = %execution_id, "workflow_completed");
            }
            Err(e) => {
                error!(execution_id = %execution_id, error = %e, "workflow_failed");
            }
        }

        result
    }

    /// Execute a single node in isolation (for debugging).
    #[instrument(
        name = "orchestrator_invoke_node",
        skip(self, payload, cancel),
        fields(
            workflow_id = %self.workflow.workflow_id,
            node_id = %node_id,
        )
    )]
    pub async fn invoke_node(
        &self,
        node_id: &str,
        payload: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<NodeResult, OrchestratorError> {
        let node = self
            .workflow
            .get_node(node_id)
            .ok_or_else(|| OrchestratorError::InvalidGraph {
                message: format!("node '{}' not found in workflow", node_id),
            })?
            .clone();

        let execution_id = uuid::Uuid::new_v4().to_string();
        let task_id = uuid::Uuid::new_v4().to_string();

        info!(
            execution_id = %execution_id,
            task_id = %task_id,
            node_id = %node_id,
            payload = %payload,
            "invoke_node_started"
        );

        let capabilities_factory = CapabilitiesFactory::new();

        let result = match &node.node_type {
            NodeType::Component(locked) => {
                // Treat payload as single upstream data for template resolution
                let mut upstream_data = HashMap::new();
                upstream_data.insert("_payload".to_string(), payload.clone());

                let resolved_strings =
                    resolve_inputs(&node.node_id, &node.inputs, &upstream_data, false)?;
                let schema = extract_schema_types(&locked.input_schema);
                let resolved_input = coerce_inputs(&node.node_id, &resolved_strings, &schema)?;

                // Resolve component bytes via component registry
                let bytes = self
                    .resolve_component_bytes(&locked.name, &locked.version)
                    .await?;

                // Build capabilities for this node
                let capabilities =
                    capabilities_factory.build_default(&execution_id, node_id);

                if cancel.is_cancelled() {
                    return Err(OrchestratorError::Cancelled);
                }

                // Execute via RuntimeRegistry
                let rt = map_runtime_type(&locked.runtime_type);
                let output = self
                    .registry
                    .execute(
                        rt,
                        &bytes,
                        capabilities,
                        RuntimeTaskInput {
                            data: resolved_input.clone(),
                        },
                    )
                    .await
                    .map_err(|e| OrchestratorError::TaskExecution { source: e })?;

                Ok(NodeResult {
                    task_id,
                    node_id: node_id.to_string(),
                    input: payload,
                    resolved_input,
                    output: output.data,
                })
            }
            NodeType::Join { .. } => Err(OrchestratorError::InvalidGraph {
                message: format!(
                    "cannot invoke_node on join node '{}' — joins are graph-level concerns",
                    node_id
                ),
            }),
            NodeType::Loop(_) => Err(OrchestratorError::InvalidGraph {
                message: format!(
                    "cannot invoke_node on loop node '{}' — loops are graph-level concerns",
                    node_id
                ),
            }),
        };

        match &result {
            Ok(node_result) => {
                info!(
                    node_id = %node_id,
                    output = %node_result.output,
                    "invoke_node_completed"
                );
            }
            Err(e) => {
                error!(node_id = %node_id, error = %e, "invoke_node_failed");
            }
        }

        result
    }

    /// Get a reference to the workflow.
    pub fn workflow(&self) -> &Workflow {
        &self.workflow
    }

    /// Resolve a component's bytes via the component registry.
    async fn resolve_component_bytes(
        &self,
        name: &str,
        version: &str,
    ) -> Result<Vec<u8>, OrchestratorError> {
        let installed = self
            .component_registry
            .get(name, Some(version))
            .await
            .map_err(|e| OrchestratorError::ComponentLoad {
                node_id: name.to_string(),
                message: format!("registry error: {}", e),
            })?
            .ok_or_else(|| OrchestratorError::ComponentLoad {
                node_id: name.to_string(),
                message: format!("component '{}@{}' not found in registry", name, version),
            })?;

        std::fs::read(&installed.wasm_path).map_err(|e| OrchestratorError::ComponentLoad {
            node_id: name.to_string(),
            message: format!("failed to read {}: {}", installed.wasm_path.display(), e),
        })
    }

    /// Run the main execution loop.
    async fn run_execution_loop(
        &self,
        completed: &mut HashMap<String, NodeResult>,
        payload: &serde_json::Value,
        execution_id: &str,
        cancel: &CancellationToken,
        capabilities_factory: &CapabilitiesFactory,
    ) -> Result<InvokeResult, OrchestratorError> {
        loop {
            if cancel.is_cancelled() {
                warn!(execution_id = %execution_id, "workflow cancelled");
                return Err(OrchestratorError::Cancelled);
            }

            let ready = self.find_ready_nodes(completed);
            if ready.is_empty() {
                break;
            }

            info!(
                execution_id = %execution_id,
                ready_nodes = ?ready,
                "executing batch of ready nodes"
            );

            // Pre-resolve component bytes for ready nodes (async)
            let mut component_bytes: HashMap<String, Vec<u8>> = HashMap::new();
            for node_id in &ready {
                if let Some(node) = self.workflow.get_node(node_id)
                    && let NodeType::Component(locked) = &node.node_type
                {
                    let bytes = self
                        .resolve_component_bytes(&locked.name, &locked.version)
                        .await?;
                    component_bytes.insert(node_id.clone(), bytes);
                }
            }

            let handles = self.execute_ready_nodes(
                &ready,
                completed,
                payload,
                execution_id,
                cancel,
                capabilities_factory,
                &component_bytes,
            )?;

            // Wait for all tasks
            let results = tokio::select! {
                results = futures::future::join_all(handles) => results,
                _ = cancel.cancelled() => {
                    warn!(execution_id = %execution_id, "workflow cancelled during task execution");
                    return Err(OrchestratorError::Cancelled);
                }
            };

            // Process results
            for result in results {
                let node_result = result
                    .map_err(|e| OrchestratorError::InvalidGraph {
                        message: format!("task join error: {}", e),
                    })?
                    .map_err(|e| {
                        error!(execution_id = %execution_id, error = %e, "task_failed");
                        e
                    })?;

                info!(
                    execution_id = %execution_id,
                    task_id = %node_result.task_id,
                    node_id = %node_result.node_id,
                    output = %node_result.output,
                    "task_completed"
                );
                completed.insert(node_result.node_id.clone(), node_result);
            }
        }

        Ok(InvokeResult {
            execution_id: execution_id.to_string(),
            node_results: completed.clone(),
        })
    }

    /// Find nodes that are ready to execute (all upstream nodes completed).
    fn find_ready_nodes(&self, completed: &HashMap<String, NodeResult>) -> Vec<String> {
        let graph = self.workflow.graph();

        self.workflow
            .nodes
            .keys()
            .filter(|id| !completed.contains_key(*id))
            .filter(|id| {
                graph
                    .upstream(id)
                    .iter()
                    .all(|up| completed.contains_key(up))
            })
            .cloned()
            .collect()
    }

    /// Spawn tasks to execute all ready nodes in parallel.
    #[allow(clippy::too_many_arguments)]
    fn execute_ready_nodes(
        &self,
        ready: &[String],
        completed: &HashMap<String, NodeResult>,
        payload: &serde_json::Value,
        execution_id: &str,
        _cancel: &CancellationToken,
        capabilities_factory: &CapabilitiesFactory,
        component_bytes: &HashMap<String, Vec<u8>>,
    ) -> Result<Vec<NodeHandle>, OrchestratorError> {
        let graph = self.workflow.graph();
        let mut handles = Vec::with_capacity(ready.len());

        for node_id in ready {
            let node_id = node_id.clone();
            let node = self
                .workflow
                .get_node(&node_id)
                .ok_or_else(|| OrchestratorError::InvalidGraph {
                    message: format!("node '{}' not found in workflow", node_id),
                })?
                .clone();
            let upstream_ids: Vec<String> = graph.upstream(&node_id).to_vec();
            let is_join = graph.is_join_point(&node_id);

            // Gather upstream data. Entry-point nodes (no upstream) receive
            // the workflow payload as if it were a single upstream output,
            // so templates like `{{ field }}` resolve against the payload.
            let upstream_data: HashMap<String, serde_json::Value> = if upstream_ids.is_empty() {
                HashMap::from([("_payload".to_string(), payload.clone())])
            } else {
                upstream_ids
                    .iter()
                    .filter_map(|id| completed.get(id).map(|r| (id.clone(), r.output.clone())))
                    .collect()
            };

            let task_id = uuid::Uuid::new_v4().to_string();

            // Build input from upstream data
            let input = if is_join {
                serde_json::Value::Object(
                    upstream_data
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            } else if upstream_data.len() == 1 {
                upstream_data
                    .values()
                    .next()
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            };

            // Resolve inputs (template rendering + type coercion)
            let resolved_input = match &node.node_type {
                NodeType::Component(locked) => {
                    let resolved_strings =
                        resolve_inputs(&node.node_id, &node.inputs, &upstream_data, is_join)?;
                    let schema = extract_schema_types(&locked.input_schema);
                    let resolved =
                        coerce_inputs(&node.node_id, &resolved_strings, &schema)?;
                    serde_json::to_value(&resolved).unwrap_or(serde_json::Value::Null)
                }
                NodeType::Join { .. } => input.clone(),
                _ => serde_json::Value::Null,
            };

            info!(
                execution_id = %execution_id,
                task_id = %task_id,
                node_id = %node_id,
                input = %input,
                resolved_input = %resolved_input,
                "task_started"
            );

            // Get pre-resolved component bytes
            let bytes = component_bytes.get(&node_id).cloned();

            // Resolve the runtime type for component nodes
            let runtime_type = match &node.node_type {
                NodeType::Component(locked) => map_runtime_type(&locked.runtime_type),
                _ => RuntimeType::Wasm, // unused for non-component nodes
            };

            // Build capabilities for this node
            let capabilities =
                capabilities_factory.build_default(execution_id, &node_id);

            let registry = Arc::clone(&self.registry);

            handles.push(tokio::spawn(async move {
                match node.node_type {
                    NodeType::Component(_) => {
                        let Some(bytes) = bytes else {
                            return Err(OrchestratorError::InvalidGraph {
                                message: "component bytes not loaded for component node"
                                    .to_string(),
                            });
                        };

                        let output = registry
                            .execute(
                                runtime_type,
                                &bytes,
                                capabilities,
                                RuntimeTaskInput {
                                    data: resolved_input.clone(),
                                },
                            )
                            .await
                            .map_err(|e| OrchestratorError::TaskExecution { source: e })?;

                        Ok(NodeResult {
                            task_id,
                            node_id: node_id.clone(),
                            input,
                            resolved_input,
                            output: output.data,
                        })
                    }
                    NodeType::Join { .. } => {
                        info!(
                            task_id = %task_id,
                            node_id = %node_id,
                            input = %input,
                            "join completed"
                        );
                        Ok(NodeResult {
                            task_id,
                            node_id: node_id.clone(),
                            input: input.clone(),
                            resolved_input,
                            output: input,
                        })
                    }
                    NodeType::Loop(_) => Err(OrchestratorError::InvalidGraph {
                        message: "loop nodes not yet implemented".to_string(),
                    }),
                }
            }));
        }

        Ok(handles)
    }

    /// Validate the workflow graph.
    fn validate_workflow(&self) -> Result<(), OrchestratorError> {
        let graph = self.workflow.graph();
        if graph.entry_points().is_empty() {
            return Err(OrchestratorError::InvalidGraph {
                message: "workflow has no entry points".to_string(),
            });
        }
        Ok(())
    }
}

/// Map from the config-layer RuntimeType to the execution-layer RuntimeType.
fn map_runtime_type(rt: &ConfigRuntimeType) -> RuntimeType {
    match rt {
        ConfigRuntimeType::Wasm => RuntimeType::Wasm,
        ConfigRuntimeType::Lua => RuntimeType::Lua,
        // JS not yet supported — fall back to Wasm
        _ => RuntimeType::Wasm,
    }
}
