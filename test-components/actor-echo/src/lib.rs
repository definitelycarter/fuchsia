//! Echo actor component for the `fuchsia-actor-wasm` integration test.
//!
//! Exports the `fuchsia:actor` lifecycle. On each `handle`, it echoes the
//! inbound payload back downstream via the `emit` import as a binary JSON blob
//! `{"echoed": <value>, "node": "<node-id>"}`. Uses only `emit` — no WASI, no
//! log, no http — so the component instantiates against a host that satisfies
//! nothing but the `fuchsia:actor` contract.

#[allow(warnings)]
mod bindings;

use bindings::exports::fuchsia::actor::actor::{Context, Guest, Payload};
use bindings::fuchsia::actor::emit;
use bindings::fuchsia::actor::types::PayloadValue;

struct Component;

impl Guest for Component {
  fn setup(_ctx: Context) -> Result<(), String> {
    Ok(())
  }

  fn handle(ctx: Context, msg: Payload) -> Result<(), String> {
    // Normalize the inbound value into a JSON fragment for the echo body.
    let inner = match msg.value {
      PayloadValue::Json(s) => s,
      PayloadValue::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
      PayloadValue::Empty => "null".to_string(),
    };

    let body = format!("{{\"echoed\": {}, \"node\": \"{}\"}}", inner, ctx.node_id);

    emit::send(&Payload {
      type_: "echo".to_string(),
      value: PayloadValue::Binary(body.into_bytes()),
    })
  }

  fn teardown(_ctx: Context) -> Result<(), String> {
    Ok(())
  }
}

bindings::export!(Component with_types_in bindings);
