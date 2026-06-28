use std::sync::Arc;

use fuchsia_actor::{Actor, ActorContext, ActorError, Emit, Message, MessageValue, async_trait};

use crate::host::LuaHost;

/// A [`fuchsia_actor::Actor`] backed by a Lua script.
///
/// Generic over a [`LuaHost`] that supplies the globals the script calls into.
/// The source, host, and emit handle are captured at construction; the
/// persistent VM is built in [`setup`](Actor::setup) and reused for every
/// [`handle`](Actor::handle) until [`teardown`](Actor::teardown).
///
/// A script must define `handle(ctx, msg)`; `setup(ctx)` and `teardown(ctx)`
/// are optional and skipped when absent.
pub struct LuaActor<H: LuaHost> {
  source: Arc<String>,
  host: Arc<H>,
  emit: Arc<dyn Emit>,
  lua: Option<mlua::Lua>,
}

impl<H: LuaHost> LuaActor<H> {
  /// Build an actor running `source`, with globals from `host`, emitting via
  /// `emit`. `source`/`host` are shared (`Arc`) across actors from one creator.
  pub fn new(source: Arc<String>, host: Arc<H>, emit: Arc<dyn Emit>) -> Self {
    Self {
      source,
      host,
      emit,
      lua: None,
    }
  }
}

#[async_trait]
impl<H: LuaHost> Actor for LuaActor<H> {
  async fn setup(&mut self, ctx: &ActorContext) -> Result<(), ActorError> {
    let lua = mlua::Lua::new();

    self
      .host
      .populate(&lua, Arc::clone(&self.emit))
      .map_err(|e| ActorError::Setup(format!("populate lua globals: {e}")))?;

    lua
      .load(self.source.as_str())
      .exec()
      .map_err(|e| ActorError::Setup(format!("load lua source: {e}")))?;

    // `handle` is required; fail fast at setup rather than per message.
    let _handle: mlua::Function = lua
      .globals()
      .get("handle")
      .map_err(|_| ActorError::Setup("script must define `handle(ctx, msg)`".to_owned()))?;

    if let Ok(setup_fn) = lua.globals().get::<mlua::Function>("setup") {
      let ctx_table = build_ctx(&lua, ctx)?;
      setup_fn
        .call::<()>(ctx_table)
        .map_err(|e| ActorError::Setup(format!("lua setup: {e}")))?;
    }

    self.lua = Some(lua);
    Ok(())
  }

  async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let lua = self
      .lua
      .as_ref()
      .ok_or_else(|| ActorError::Handle("lua vm not initialized".to_owned()))?;

    let ctx_table = build_ctx(lua, ctx)?;
    let msg_table = build_msg(lua, &msg)?;

    let handle_fn: mlua::Function = lua
      .globals()
      .get("handle")
      .map_err(|e| ActorError::Handle(format!("lookup handle: {e}")))?;

    handle_fn
      .call::<()>((ctx_table, msg_table))
      .map_err(|e| ActorError::Handle(format!("lua handle: {e}")))
  }

  async fn teardown(&mut self, ctx: &ActorContext) -> Result<(), ActorError> {
    // Best-effort: nothing to do if setup never built the VM.
    let Some(lua) = self.lua.as_ref() else {
      return Ok(());
    };
    if let Ok(teardown_fn) = lua.globals().get::<mlua::Function>("teardown") {
      match build_ctx(lua, ctx).and_then(|c| {
        teardown_fn
          .call::<()>(c)
          .map_err(|e| ActorError::Teardown(format!("lua teardown: {e}")))
      }) {
        Ok(()) => {}
        Err(e) => tracing::warn!(error = %e, "lua teardown error"),
      }
    }
    Ok(())
  }
}

/// Build the `ctx` table handed to a script lifecycle function.
fn build_ctx(lua: &mlua::Lua, ctx: &ActorContext) -> Result<mlua::Table, ActorError> {
  let table = lua
    .create_table()
    .map_err(|e| ActorError::Handle(format!("lua ctx table: {e}")))?;
  let set = |k: &str, v: &str| {
    table
      .set(k, v)
      .map_err(|e| ActorError::Handle(format!("lua ctx set {k}: {e}")))
  };
  set("execution_id", &ctx.execution_id)?;
  set("node_id", &ctx.node_id)?;
  set("task_id", &ctx.task_id)?;
  Ok(table)
}

/// Build the `msg` table: `{ type = ..., value = { kind = ..., data = ... } }`.
/// `kind` is `"json" | "binary" | "empty"`; for json, `data` is the JSON text;
/// for binary, `data` is a Lua string of the bytes.
fn build_msg(lua: &mlua::Lua, msg: &Message) -> Result<mlua::Table, ActorError> {
  let table = lua
    .create_table()
    .map_err(|e| ActorError::Handle(format!("lua msg table: {e}")))?;
  table
    .set("type", msg.type_.as_str())
    .map_err(|e| ActorError::Handle(format!("lua msg set type: {e}")))?;

  let value = lua
    .create_table()
    .map_err(|e| ActorError::Handle(format!("lua msg value table: {e}")))?;
  match &msg.value {
    MessageValue::Json(v) => {
      let json = serde_json::to_string(v)
        .map_err(|e| ActorError::Handle(format!("encode msg json: {e}")))?;
      value
        .set("kind", "json")
        .and_then(|_| value.set("data", json))
        .map_err(|e| ActorError::Handle(format!("lua msg value: {e}")))?;
    }
    MessageValue::Binary(b) => {
      let bytes = lua
        .create_string(b)
        .map_err(|e| ActorError::Handle(format!("lua msg bytes: {e}")))?;
      value
        .set("kind", "binary")
        .and_then(|_| value.set("data", bytes))
        .map_err(|e| ActorError::Handle(format!("lua msg value: {e}")))?;
    }
    MessageValue::Empty => {
      value
        .set("kind", "empty")
        .map_err(|e| ActorError::Handle(format!("lua msg value: {e}")))?;
    }
  }
  table
    .set("value", value)
    .map_err(|e| ActorError::Handle(format!("lua msg set value: {e}")))?;
  Ok(table)
}
