//! Fuchsia Workflow Orchestrator
//!
//! This crate handles workflow execution: DAG traversal, wave-based scheduling,
//! input resolution, and component execution dispatch. It delegates all task
//! execution to the `RuntimeRegistry` from `fuchsia-task-runtime`, making the
//! orchestrator fully runtime-agnostic. Workflows are invoked manually with a
//! payload that is fed to graph entry points.

mod capabilities;
mod error;
mod input;
mod orchestrator;
mod result;

pub use capabilities::CapabilitiesFactory;
pub use error::OrchestratorError;
pub use input::{SchemaType, coerce_inputs, extract_schema_types, resolve_inputs};
pub use orchestrator::{Orchestrator, OrchestratorConfig};
pub use result::{InvokeResult, NodeResult};
