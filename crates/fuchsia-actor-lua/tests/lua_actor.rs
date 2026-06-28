//! End-to-end: register a `LuaActorCreator<BaseLuaHost>` as the `"lua"` runtime
//! on the engine, provision a two-node graph (lua echo → native recorder),
//! push a message into the lua node, and assert the script echoed it onward
//! through `emit` to the recorder.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  MessageValue,
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

impl Actor for Recorder {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.out.lock().expect("recorder lock").push(msg);
    self.notify.notify_one();
    Ok(())
  }
  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
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
    .add_edge(lua_id.clone(), rec_id.clone())
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
