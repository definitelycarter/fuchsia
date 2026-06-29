//! End-to-end: register a `LuaActorCreator<BaseLuaHost>` as the `"lua"` runtime
//! on the engine, provision a two-node graph (lua echo → native recorder),
//! push a message into the lua node, and assert the script echoed it onward
//! through `emit` to the recorder.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  MessageValue, async_trait,
};
use fuchsia_actor_lua::{BaseLuaHost, LuaActorCreator};
use fuchsia_engine::Engine;
use tokio::sync::Notify;

const SCRIPT: &str = r#"
function handle(ctx, msg)
  local data = (msg.value and msg.value.data) or "null"
  emit({
    type = "echo",
    value = {
      kind = "json",
      data = string.format('{"echoed": %s, "node": "%s"}', data, ctx.node_id)
    }
  })
end
"#;

// ---- A native recorder actor that captures what it receives ----------------

struct Recorder {
  out: Arc<Mutex<Vec<Message>>>,
  notify: Arc<Notify>,
}

#[async_trait]
impl Actor for Recorder {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.out.lock().expect("recorder lock").push(msg);
    self.notify.notify_one();
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct RecorderCreator {
  out: Arc<Mutex<Vec<Message>>>,
  notify: Arc<Notify>,
}

impl ActorCreator for RecorderCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Recorder {
      out: self.out.clone(),
      notify: self.notify.clone(),
    }))
  }
}

#[tokio::test]
async fn lua_script_echoes_through_a_provisioned_graph() {
  let creator = LuaActorCreator::new(BaseLuaHost::new()).with_source("echo", SCRIPT);

  let out = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Engine::new();
  engine.register("lua", creator).await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        out: out.clone(),
        notify: notify.clone(),
      },
    )
    .await;

  let lua_id = ActorId::scoped("wf", "lua");
  let rec_id = ActorId::scoped("wf", "rec");

  // The script id rides in env, exactly as a host writes it.
  let mut env = std::collections::BTreeMap::new();
  env.insert("component".to_owned(), "echo".to_owned());
  let lua_cfg = ActorConfig {
    env,
    settings: Default::default(),
  };

  engine
    .add_node(lua_id.clone(), "lua", &lua_cfg, ActorCapabilities::new())
    .await
    .expect("add lua node");
  engine
    .add_node(
      rec_id.clone(),
      "recorder",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .expect("add recorder node");
  engine
    .add_default_edge(lua_id.clone(), rec_id.clone())
    .expect("add edge");

  engine
    .push(&lua_id, Message::json("test", serde_json::json!(42)))
    .expect("push");

  tokio::time::timeout(Duration::from_secs(5), notify.notified())
    .await
    .expect("recorder received an emission");

  let recorded = out.lock().expect("recorder lock");
  assert_eq!(recorded.len(), 1, "expected one emission, got {recorded:?}");

  let MessageValue::Json(v) = &recorded[0].value else {
    panic!("expected json message, got {:?}", recorded[0].value);
  };
  assert_eq!(v["echoed"], serde_json::json!(42));
  assert_eq!(v["node"], serde_json::json!("wf/lua"));
  assert_eq!(recorded[0].type_, "echo");
}

/// A script that branches via the `emit_to(port, msg)` global: `>30` goes to
/// the `"hot"` port, else `"cold"`.
const BRANCH_SCRIPT: &str = r#"
function handle(ctx, msg)
  local temp = tonumber(msg.value.data)
  if temp > 30 then
    emit_to("hot", { type = "alert", value = { kind = "empty" } })
  else
    emit_to("cold", { type = "ok", value = { kind = "empty" } })
  end
end
"#;

#[tokio::test]
async fn lua_emit_to_routes_per_named_port() {
  let creator = LuaActorCreator::new(BaseLuaHost::new()).with_source("branch", BRANCH_SCRIPT);

  let hot = Arc::new(Mutex::new(Vec::new()));
  let cold = Arc::new(Mutex::new(Vec::new()));
  let hot_notify = Arc::new(Notify::new());
  let cold_notify = Arc::new(Notify::new());

  let engine = Engine::new();
  engine.register("lua", creator).await;
  engine
    .register(
      "hot_rec",
      RecorderCreator {
        out: hot.clone(),
        notify: hot_notify.clone(),
      },
    )
    .await;
  engine
    .register(
      "cold_rec",
      RecorderCreator {
        out: cold.clone(),
        notify: cold_notify.clone(),
      },
    )
    .await;

  let lua_id = ActorId::scoped("wf", "branch");
  let hot_id = ActorId::scoped("wf", "hot");
  let cold_id = ActorId::scoped("wf", "cold");

  let mut env = std::collections::BTreeMap::new();
  env.insert("component".to_owned(), "branch".to_owned());
  let lua_cfg = ActorConfig {
    env,
    settings: Default::default(),
  };

  engine
    .add_node(lua_id.clone(), "lua", &lua_cfg, ActorCapabilities::new())
    .await
    .expect("add lua node");
  engine
    .add_node(
      hot_id.clone(),
      "hot_rec",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .expect("add hot recorder");
  engine
    .add_node(
      cold_id.clone(),
      "cold_rec",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .expect("add cold recorder");
  // Lua is a Dynamic node, so its emit-time ports wire freely.
  engine
    .add_edge(lua_id.clone(), "hot", hot_id.clone())
    .expect("edge hot");
  engine
    .add_edge(lua_id.clone(), "cold", cold_id.clone())
    .expect("edge cold");

  // 42 > 30 → the "hot" port only.
  engine
    .push(&lua_id, Message::json("reading", serde_json::json!(42)))
    .expect("push");

  tokio::time::timeout(Duration::from_secs(5), hot_notify.notified())
    .await
    .expect("hot recorder received");

  assert_eq!(hot.lock().expect("hot lock").len(), 1);
  assert!(cold.lock().expect("cold lock").is_empty());
}
