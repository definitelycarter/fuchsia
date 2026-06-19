//! Wasm-component-hosting [`Actor`] implementation for fuchsia.
//!
//! A [`WasmActor`] drives a wasm component's `fuchsia:actor` lifecycle
//! (`setup`/`handle`/`teardown`) on the new handle-per-message model: the
//! runtime owns the receive loop and calls the actor's synchronous lifecycle
//! methods, each of which trampolines into the component.
//!
//! # Synchronous by design
//!
//! The `fuchsia:actor` contract — lifecycle plus the host-imported `emit` — is
//! entirely synchronous (emit is a non-blocking channel `offer`). So this crate
//! uses **synchronous** wasmtime: no `async_support`, no `block_on`, no fibers.
//! A component call runs to completion on the runtime task driving it, exactly
//! like a native actor's `handle`. Product capabilities that are inherently
//! async (HTTP, etc.) are *not* part of this contract — a product host wires
//! them into its own [`WasmHost`] and decides how to bridge them.
//!
//! # Generic over the import set
//!
//! fuchsia owns only the `fuchsia:actor` contract; it does not prescribe which
//! capabilities a component may import. [`WasmActor`] is therefore generic over
//! [`WasmHost`], the seam a product implements to wire its own world's imports
//! into the linker. This crate ships [`BaseHost`], which satisfies nothing but
//! the contract (it links `emit` and traps any other import the component
//! happens to carry, e.g. unused WASI) — enough to run a component that uses
//! only `fuchsia:actor`.
//!
//! [`Actor`]: fuchsia_actor::Actor

mod actor;
mod base;
mod creator;
mod host;

pub use actor::WasmActor;
pub use base::{BaseHost, BaseHostState};
pub use creator::WasmActorCreator;
pub use host::WasmHost;
