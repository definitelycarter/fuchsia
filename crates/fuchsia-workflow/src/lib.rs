//! Fuchsia Workflow
//!
//! This crate provides the "locked" workflow representation for Fuchsia.
//! A locked workflow is a validated, resolved form of a workflow configuration
//! that is ready for execution.
//!
//! Key differences from `fuchsia-config`:
//! - Graph structure is validated (no orphans, valid edges)
//! - Components are referenced by digest (content-addressable)
//! - Entry points and join points are identified
//! - Ready to be dispatched to workers for execution

mod error;
mod graph;
mod node;
mod workflow;

pub use error::WorkflowError;
pub use graph::Graph;
pub use node::{LockedComponent, LockedLoop, Node, NodeType};
pub use workflow::Workflow;
