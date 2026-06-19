//! Lua-script-hosting [`Actor`] implementation for fuchsia.
//!
//! A [`LuaActor`] drives a Lua script's lifecycle on the handle-per-message
//! model: the runtime owns the receive loop and calls the actor's synchronous
//! lifecycle methods, each of which calls into the persistent Lua VM. A script
//! must define `handle(ctx, msg)`; `setup(ctx)` and `teardown(ctx)` are
//! optional globals.
//!
//! # Synchronous by design
//!
//! The contract — lifecycle plus the `emit` global — is synchronous (emit is a
//! non-blocking channel `offer`), so the VM is driven directly with no
//! `block_on`. Product capabilities that are inherently async are the product
//! host's concern, exactly as on the wasm side.
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
