//! Workflow definitions: the node graph a provisioner reads to build a running
//! engine graph.
//!
//! This crate owns only the *shape* of a workflow — plain serde/BSON types.
//! Persistence (opening a database, transactions, CRUD) is deliberately not
//! here: it is a downstream product concern, layered over these types under
//! whatever store the product chooses.

mod node;
mod workflow;

pub use node::{BuiltinConfig, ComponentConfig, Edge, Node, NodeDefinition, NodeId, Runtime};
pub use workflow::{Workflow, WorkflowId};
