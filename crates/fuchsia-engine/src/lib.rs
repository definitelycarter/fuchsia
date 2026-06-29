//! The fuchsia workflow engine: routes emissions between actors according to a
//! graph's edges.
//!
//! It sits above `fuchsia-runtime` (the actor substrate) and knows only **actors
//! and addressing** — it instantiates nodes, wires each node's `emit` to its
//! successors' mailboxes (lookup through a live routing table, not baked), and
//! delivers. It has no knowledge of entities, workflow definitions, or their
//! bindings; an assembler above it translates those into the nodes and edges.

mod engine;
mod error;
mod router;

pub use engine::Engine;
pub use error::EngineError;
pub use router::RouteCounts;

/// The run id a trigger mints for [`Engine::push`] / [`Engine::push_durable`].
/// Re-exported so callers reach it through the engine surface they already use.
pub use fuchsia_transport::CorrelationId;
