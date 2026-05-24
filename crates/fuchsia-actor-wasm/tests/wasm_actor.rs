//! End-to-end integration test: load the test wasm component, register a
//! `WasmActor<DefaultHost>` with `fuchsia-runtime`, push a typed payload through,
//! and assert the component echoed it back.
//!
//! Requires `cargo component build --release` to have been run in
//! `test-components/test-actor-component` first.

use async_trait::async_trait;
use fuchsia_actor::{Actor, ActorError, Context, Emitter, Inbox, Message, MessageValue};
use fuchsia_actor_wasm::{DefaultHost, WasmActor};
use fuchsia_capabilities::http::{AllowedHosts, ReqwestHttp};
use fuchsia_runtime::{ActorRegistry, Edge, Graph, Node, Orchestrator};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};

const TEST_WASM: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../test-components/test-actor-component/target/wasm32-wasip1/release/test_actor_component.wasm"
);

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

#[tokio::test]
async fn wasm_actor_runs_test_component_end_to_end() {
  let wasm_path = Path::new(TEST_WASM);
  if !wasm_path.exists() {
    panic!(
      "Test component not found at {}. Run `cargo component build --release` \
       in test-components/test-actor-component first.",
      wasm_path.display()
    );
  }

  let mut config = wasmtime::Config::new();
  config.async_support(true);
  config.wasm_component_model(true);
  let engine = wasmtime::Engine::new(&config).expect("create wasmtime engine");

  // The test component doesn't actually make HTTP calls, but DefaultHost
  // requires an HttpClient to satisfy the world's `http` import. Provide
  // a real client with an empty allow-list — any HTTP call from a component
  // under test would be rejected with HostNotAllowed.
  let http = Arc::new(ReqwestHttp::new(AllowedHosts::default()));
  let host = DefaultHost::new(http);

  let actor = WasmActor::builder(engine, host)
    .component_from_path(wasm_path)
    .build()
    .expect("build WasmActor");

  let out = Arc::new(Mutex::new(Vec::new()));

  let mut registry = ActorRegistry::new();
  {
    let actor = actor.clone();
    registry.register::<WasmActor<DefaultHost>, Value, _>("test.wasm", move |_| actor.clone());
  }
  {
    let out = out.clone();
    registry.register::<Recorder, Value, _>("recorder", move |_| Recorder { out: out.clone() });
  }

  let graph = Graph {
    entry: "wasm".into(),
    nodes: vec![
      Node {
        id: "wasm".into(),
        actor: "test.wasm".into(),
        config: Value::Null,
      },
      Node {
        id: "rec".into(),
        actor: "recorder".into(),
        config: Value::Null,
      },
    ],
    edges: vec![Edge {
      from: "wasm".into(),
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

  // test-actor-component echoes back: {"echoed": <json>, "node": "<id>"}
  let MessageValue::Json(v) = &recorded[0].value else {
    panic!("expected JSON message");
  };
  assert_eq!(v["echoed"], json!(42));
  assert_eq!(v["node"], json!("wasm"));
}
