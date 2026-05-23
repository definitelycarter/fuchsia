//! Generated bindings for fuchsia host world.
//!
//! This crate uses wasmtime's bindgen to generate Rust bindings from WIT interfaces.
//! The host world combines all imports the host provides and exports the host calls.

wasmtime::component::bindgen!({
    path: "../../wit",
    inline: r#"
        package fuchsia:runtime;

        world host {
            include fuchsia:fuchsia/host;
        }
    "#,
    imports: { default: async },
    exports: { default: async },
});
