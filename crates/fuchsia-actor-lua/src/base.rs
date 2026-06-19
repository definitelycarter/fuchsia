//! [`BaseLuaHost`] — a [`LuaHost`] that registers only the contract `emit`
//! global. It wires no platform capabilities (no log, no http) — those belong
//! to product-defined hosts. Enough to run a script that only emits.

use std::sync::Arc;

use fuchsia_actor::{Emit, Message, MessageValue};

use crate::host::LuaHost;

/// Contract-only [`LuaHost`]: registers `emit(msg)` and nothing else.
///
/// Scripts emit with:
/// ```lua
/// emit({ type = "echo", value = { kind = "json", data = "42" } })
/// ```
/// `kind` is `"json" | "binary" | "empty"`. The emission is best-effort (a
/// non-blocking channel offer), so `emit` always returns successfully.
#[derive(Default)]
pub struct BaseLuaHost;

impl BaseLuaHost {
  pub fn new() -> Self {
    Self
  }
}

impl LuaHost for BaseLuaHost {
  fn populate(&self, lua: &mlua::Lua, emit: Arc<dyn Emit>) -> mlua::Result<()> {
    register_emit(lua, emit)
  }
}

fn register_emit(lua: &mlua::Lua, emit: Arc<dyn Emit>) -> mlua::Result<()> {
  let emit_fn = lua.create_function(move |_, msg: mlua::Table| {
    emit.emit(table_to_message(&msg)?);
    Ok(())
  })?;
  lua.globals().set("emit", emit_fn)
}

/// Convert an emitted Lua table into a [`Message`].
fn table_to_message(msg: &mlua::Table) -> mlua::Result<Message> {
  let type_: String = msg.get("type").unwrap_or_else(|_| "emit".to_owned());

  let value = match msg.get::<mlua::Table>("value") {
    Ok(value) => {
      let kind: String = value.get("kind").unwrap_or_else(|_| "empty".to_owned());
      match kind.as_str() {
        "json" => {
          let data: String = value.get("data").unwrap_or_else(|_| "null".to_owned());
          let json = serde_json::from_str(&data)
            .map_err(|e| mlua::Error::external(format!("emit: invalid JSON: {e}")))?;
          MessageValue::Json(json)
        }
        "binary" => {
          let data: mlua::String = value.get("data")?;
          MessageValue::Binary(data.as_bytes().to_vec())
        }
        _ => MessageValue::Empty,
      }
    }
    Err(_) => MessageValue::Empty,
  };

  Ok(Message { type_, value })
}
