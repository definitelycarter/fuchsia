//! Lua-script-hosting [`Actor`] implementation for fuchsia.
//!
//! A [`LuaActor`] drives a Lua script's lifecycle on the handle-per-message
//! model: the runtime owns the receive loop and calls the actor's async
//! lifecycle methods, each of which drives the persistent Lua VM via mlua's
//! `call_async`. A script must define `handle(ctx, msg)`; `setup(ctx)` and
//! `teardown(ctx)` are optional globals.
//!
//! # Synchronous guest, async host
//!
//! The Lua *script* contract stays synchronous — authors write straight-line
//! code. The host drives it via mlua `call_async`, so a script that calls an
//! async host global (registered with [`mlua::Lua::create_async_function`])
//! suspends and yields the runtime thread instead of blocking it. The `emit`
//! global stays a synchronous, non-blocking channel `offer`.
//!
//! # Generic over the binding set
//!
//! [`LuaActor`] is generic over [`LuaHost`] — the analog of the wasm side's
//! `WasmHost::add_to_linker`, just without WIT. A host populates the Lua state
//! with whatever globals its scripts may call. This crate ships [`BaseLuaHost`],
//! which registers only the contract `emit` global; richer hosts register their
//! own capability globals.
//!
//! [`Actor`]: fuchsia_actor::Actor

mod actor;
mod base;
mod creator;
mod host;

pub use actor::LuaActor;
pub use base::BaseLuaHost;
pub use creator::LuaActorCreator;
pub use host::LuaHost;

/// Re-exported so a product can implement [`LuaHost`] (whose methods take
/// `&mlua::Lua`) without separately depending on — and version-matching — mlua.
pub use mlua;
