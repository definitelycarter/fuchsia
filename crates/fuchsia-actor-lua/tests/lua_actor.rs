//! End-to-end integration test: register a `LuaActor<DefaultLuaHost>` with
//! `fuchsia-runtime`, push a typed payload through, and assert the script
//! echoed it back.

use async_trait::async_trait;
use fuchsia_actor::{Actor, ActorError, Context, Emitter, Inbox, Message, MessageValue};
use fuchsia_actor_lua::{DefaultLuaHost, LuaActor};
use fuchsia_capabilities::http::{AllowedHosts, ReqwestHttp};
use fuchsia_runtime::{ActorRegistry, Edge, Graph, Node, Orchestrator};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

struct Recorder {
  out: Arc<Mutex<Vec<Message>>>,
}

#[async_trait]
impl Actor for Recorder {
  async fn run(&self, mut inbox: Inbox, _emit: Emitter, ctx: Context) -> Result<(), ActorError> {
    loop {
      tokio::select! {
          _ = ctx.cancelled() => return Ok(()),
          msg = inbox.recv() => match msg {
              Some(msg) => self.out.lock().unwrap().push(msg),
              None => return Ok(()),
          }
      }
    }
  }
}

const SCRIPT: &str = r#"
function handle(ctx, msg)
  log.log("info", "lua: node " .. ctx.node_id .. " received type " .. msg.type)
  local data = (msg.value and msg.value.data) or "null"
  emit({
    type = "echo",
    value = { kind = "json", data = string.format('{"echoed": %s, "node": "%s"}', data, ctx.node_id) }
  })
end
"#;

#[tokio::test]
async fn lua_actor_runs_inline_script_end_to_end() {
  // No outbound HTTP in this test, but DefaultLuaHost needs an HttpClient
  // to wire the http global. Empty allow-list rejects any call.
  let http = Arc::new(ReqwestHttp::new(AllowedHosts::default()));
  let host = DefaultLuaHost::new(http);

  let actor = LuaActor::builder(host)
    .source(SCRIPT)
    .build()
    .expect("build LuaActor");

  let out = Arc::new(Mutex::new(Vec::new()));

  let mut registry = ActorRegistry::new();
  {
    let actor = actor.clone();
    registry.register::<LuaActor<DefaultLuaHost>, Value, _>("test.lua", move |_| actor.clone());
  }
  {
    let out = out.clone();
    registry.register::<Recorder, Value, _>("recorder", move |_| Recorder { out: out.clone() });
  }

  let graph = Graph {
    entry: "lua".into(),
    nodes: vec![
      Node {
        id: "lua".into(),
        actor: "test.lua".into(),
        config: Value::Null,
      },
      Node {
        id: "rec".into(),
        actor: "recorder".into(),
        config: Value::Null,
      },
    ],
    edges: vec![Edge {
      from: "lua".into(),
      to: "rec".into(),
    }],
  };

  let orch = Orchestrator::new(Arc::new(registry));
  let handle = orch.start(&graph).expect("start workflow");

  handle
    .send(Message::json("test", json!(42)))
    .await
    .expect("send input");

  let results = handle.join().await;
  for (i, r) in results.iter().enumerate() {
    assert!(r.is_ok(), "actor {i} failed: {r:?}");
  }

  let recorded = out.lock().unwrap();
  assert_eq!(recorded.len(), 1, "expected one output, got {recorded:?}");

  let MessageValue::Json(v) = &recorded[0].value else {
    panic!("expected JSON message, got {:?}", recorded[0].type_);
  };
  assert_eq!(v["echoed"], json!(42));
  assert_eq!(v["node"], json!("lua"));
}
