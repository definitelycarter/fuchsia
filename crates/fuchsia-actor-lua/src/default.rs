//! `DefaultLuaHost` — built-in [`LuaHost`] for the canonical capability set
//! (log + http + emit). Mirrors [`fuchsia_actor_wasm::DefaultHost`].

use crate::host::LuaHost;
use fuchsia_actor::{Emitter, Message, MessageValue};
use fuchsia_capabilities::http::{HttpClient, HttpError, HttpRequest};
use std::collections::HashMap;
use std::sync::Arc;

/// Built-in [`LuaHost`] for the canonical capability set.
///
/// Registers three globals on the Lua state:
///
/// - `log.log(level, message)` — routes to `tracing` under target `"lua.actor"`.
///   `level` is `"trace" | "debug" | "info" | "warn" | "error"`.
/// - `http.send({ method, url, headers, body }) -> { status, headers, body }` —
///   delegates to the injected [`HttpClient`]. Errors are surfaced as Lua
///   runtime errors with the underlying `HttpError` display.
/// - `emit(data)` — pushes a JSON-encoded payload to the actor's outbound
///   channel. Returns a Lua error if the downstream channel is closed.
#[derive(Clone)]
pub struct DefaultLuaHost {
  http: Arc<dyn HttpClient>,
}

impl DefaultLuaHost {
  pub fn new(http: Arc<dyn HttpClient>) -> Self {
    Self { http }
  }
}

impl LuaHost for DefaultLuaHost {
  fn populate(&self, lua: &mlua::Lua, emitter: Emitter) -> mlua::Result<()> {
    register_log(lua)?;
    register_http(lua, Arc::clone(&self.http))?;
    register_emit(lua, emitter)?;
    Ok(())
  }
}

fn register_log(lua: &mlua::Lua) -> mlua::Result<()> {
  let table = lua.create_table()?;
  table.set(
    "log",
    lua.create_function(|_, (level, message): (String, String)| {
      match level.as_str() {
        "trace" => tracing::trace!(target: "lua.actor", "{message}"),
        "debug" => tracing::debug!(target: "lua.actor", "{message}"),
        "info" => tracing::info!(target: "lua.actor", "{message}"),
        "warn" => tracing::warn!(target: "lua.actor", "{message}"),
        "error" => tracing::error!(target: "lua.actor", "{message}"),
        _ => tracing::info!(target: "lua.actor", "{message}"),
      }
      Ok(())
    })?,
  )?;
  lua.globals().set("log", table)?;
  Ok(())
}

fn register_http(lua: &mlua::Lua, http: Arc<dyn HttpClient>) -> mlua::Result<()> {
  let table = lua.create_table()?;
  let http_for_send = Arc::clone(&http);
  table.set(
    "send",
    lua.create_function(move |lua_inner, req: mlua::Table| {
      let request = lua_table_to_request(req)?;
      let result = futures::executor::block_on(http_for_send.send(request));

      match result {
        Ok(resp) => {
          let table = lua_inner.create_table()?;
          table.set("status", resp.status)?;
          let headers = lua_inner.create_table()?;
          for (k, v) in resp.headers {
            headers.set(k, v)?;
          }
          table.set("headers", headers)?;
          table.set("body", resp.body)?;
          Ok(table)
        }
        Err(err) => Err(mlua::Error::external(LuaHttpError(err))),
      }
    })?,
  )?;
  lua.globals().set("http", table)?;
  Ok(())
}

fn register_emit(lua: &mlua::Lua, emitter: Emitter) -> mlua::Result<()> {
  let emit_fn = lua.create_function(move |_, msg: mlua::Table| {
    let type_: String = msg.get("type").unwrap_or_else(|_| "emit".to_string());
    let value = if let Ok(value_table) = msg.get::<mlua::Table>("value") {
      let kind: String = value_table
        .get("kind")
        .unwrap_or_else(|_| "empty".to_string());
      match kind.as_str() {
        "json" => {
          let data: String = value_table
            .get("data")
            .unwrap_or_else(|_| "null".to_string());
          let json_val = serde_json::from_str(&data)
            .map_err(|e| mlua::Error::external(format!("emit: invalid JSON: {e}")))?;
          MessageValue::Json(json_val)
        }
        "binary" => {
          let data: mlua::String = value_table.get("data")?;
          MessageValue::Binary(data.as_bytes().to_vec())
        }
        _ => MessageValue::Empty,
      }
    } else {
      MessageValue::Empty
    };
    let message = Message { type_, value };
    futures::executor::block_on(emitter.send(message))
      .map_err(|_| mlua::Error::external("channel closed".to_string()))?;
    Ok(())
  })?;
  lua.globals().set("emit", emit_fn)?;
  Ok(())
}

fn lua_table_to_request(table: mlua::Table) -> mlua::Result<HttpRequest> {
  let method: String = table.get("method")?;
  let url: String = table.get("url")?;
  let body: Option<String> = table.get("body").ok();

  let mut headers = HashMap::new();
  if let Ok(headers_table) = table.get::<mlua::Table>("headers") {
    for pair in headers_table.pairs::<String, String>() {
      let (k, v) = pair?;
      headers.insert(k, v);
    }
  }

  Ok(HttpRequest {
    method,
    url,
    headers,
    body,
  })
}

/// Newtype wrapper so [`HttpError`] can flow through `mlua::Error::external`.
#[derive(Debug)]
struct LuaHttpError(HttpError);

impl std::fmt::Display for LuaHttpError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.0)
  }
}

impl std::error::Error for LuaHttpError {}
