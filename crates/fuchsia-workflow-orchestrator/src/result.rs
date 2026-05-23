//! Execution result types.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Result of a single node execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResult {
    /// Unique task ID for this execution.
    pub task_id: String,
    /// Node ID that was executed.
    pub node_id: String,
    /// Input from upstream node(s).
    pub input: serde_json::Value,
    /// Input after template resolution.
    pub resolved_input: serde_json::Value,
    /// Node output.
    pub output: serde_json::Value,
}

/// Result of a complete workflow invocation.
#[derive(Debug, Serialize, Deserialize)]
pub struct InvokeResult {
    /// Unique execution ID.
    pub execution_id: String,
    /// Results of all executed nodes, keyed by node_id.
    pub node_results: HashMap<String, NodeResult>,
}
