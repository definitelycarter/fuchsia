use std::sync::Arc;

use fuchsia_actor::Emit;

/// Glue between [`LuaActor`](crate::LuaActor) and a host-specific binding set.
///
/// A `LuaHost` owns the globals registered into the Lua state — the analog of
/// the wasm side's [`WasmHost::add_to_linker`], without WIT. The actor crate
/// provides the lifecycle orchestration; the host populates the VM with the
/// globals scripts are expected to call. This is the seam that keeps the crate
/// generic over the capability set.
///
/// Implementations must register an `emit(msg)` global that forwards into the
/// provided [`Emit`] sink — that is how scripts push downstream payloads.
///
/// This crate ships [`BaseLuaHost`](crate::BaseLuaHost) for the contract-only
/// set (just `emit`). Hosts with more capabilities (log, http, …) implement
/// `LuaHost` themselves.
///
/// [`WasmHost::add_to_linker`]: https://docs.rs/fuchsia-actor-wasm
pub trait LuaHost: 'static + Send + Sync {
  /// Populate the Lua state with host-provided globals. Called once per actor,
  /// before the script source is loaded. `emit` is the actor's outbound sink —
  /// implementations must wire it into an `emit` global the script can call.
  fn populate(&self, lua: &mlua::Lua, emit: Arc<dyn Emit>) -> mlua::Result<()>;
}
