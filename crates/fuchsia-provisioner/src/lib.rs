//! The provisioner: turns workflow *definitions* into running graphs on the
//! engine, and owns the domain↔actor bindings.
//!
//! It sits above `fuchsia-engine` (which knows only actors + addressing) and
//! holds a shared `Arc<Engine>`. `register_workflow` translates a stored
//! workflow into a grouped graph (group = the workflow's id).

mod error;
mod provisioner;

pub use error::ProvisionerError;
pub use provisioner::Provisioner;
