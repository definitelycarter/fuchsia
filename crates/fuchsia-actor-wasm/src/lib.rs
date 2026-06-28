//! Wasm-component-hosting [`Actor`] implementation for fuchsia.
//!
//! A [`WasmActor`] drives a wasm component's `fuchsia:actor` lifecycle
//! (`setup`/`handle`/`teardown`) on the handle-per-message model: the runtime
//! owns the receive loop and calls the actor's async lifecycle methods, each of
//! which drives the component via wasmtime's async exports (`call_async`).
//!
//! # Synchronous guest, async host
//!
//! The `fuchsia:actor` *WIT contract* stays synchronous — guest authors write
//! straight-line code, and `emit` is a synchronous, non-blocking channel
//! `offer`. The host drives the component with **async wasmtime**
//! (`exports: async`, `call_async`): when a guest calls an async host import (a
//! product's `fetch`, say) wasmtime suspends the guest fiber until the Rust
//! future resolves, yielding the runtime thread rather than blocking it. Product
//! capabilities that are inherently async are wired into a product's own
//! [`WasmHost`].
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

/// Re-exported so a product can implement [`WasmHost`] (whose methods take
/// `wasmtime` types) without separately depending on — and version-matching —
/// wasmtime.
pub use wasmtime;
