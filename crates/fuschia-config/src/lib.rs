//! Fuschia Config
//!
//! This crate contains the serializable workflow configuration types for Fuschia.
//! These types represent workflow definitions before they are loaded and resolved
//! by the engine.
//!
//! Configuration can be loaded from:
//! - JSON files (via CLI with `--config=workflow.json`)
//! - Database storage (as JSON blobs)
//!
//! The engine takes these configuration types, validates them against component
//! schemas, and resolves them into runtime structures for execution.

mod component;
mod edge;
mod enums;
mod input;
mod node;
mod workflow;

pub use component::ComponentRef;
pub use edge::Edge;
pub use enums::{ExecutionMode, JoinStrategy, LoopFailureMode, RetryBackoff, RuntimeType};
pub use input::InputValue;
pub use node::{NodeDef, NodeType};
pub use workflow::WorkflowDef;
