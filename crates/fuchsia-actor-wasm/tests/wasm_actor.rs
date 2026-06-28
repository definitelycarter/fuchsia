//! End-to-end: register a `WasmActorCreator<BaseHost>` as the `"wasm"` runtime
//! on the engine, provision a two-node graph (wasm echo → native recorder),
//! push a message into the wasm node, and assert the component echoed it onward
//! through `emit` to the recorder.
//!
//! Requires the test component to be built first:
//!   (cd test-components/actor-echo && cargo component build --release)
//! If the artifact is absent the test skips (keeps `cargo test --workspace`
//! green without the wasm toolchain step).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  MessageValue, async_trait,
};
use fuchsia_actor_wasm::{BaseHost, WasmActorCreator};
use fuchsia_engine::Engine;
use tokio::sync::Notify;

const TEST_WASM: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../test-components/actor-echo/target/wasm32-wasip1/release/actor_echo.wasm"
);

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
async fn wasm_component_echoes_through_a_provisioned_graph() {
  if !std::path::Path::new(TEST_WASM).exists() {
    eprintln!(
      "skipping: {TEST_WASM} not found — run `cargo component build --release` \
       in test-components/actor-echo first"
    );
    return;
  }

  // Register the "wasm" runtime with a catalog holding the echo component.
  let creator = WasmActorCreator::new(BaseHost::new())
    .expect("build wasm creator")
    .with_path("echo", TEST_WASM)
    .expect("load echo component");

  let out = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Engine::new();
  engine.register("wasm", creator).await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        out: out.clone(),
        notify: notify.clone(),
      },
    )
    .await;

  // Provision the graph: the wasm node's `component` id rides in env, exactly
  // as a host writes it for a `Component` node.
  let wasm_id = ActorId::scoped("wf", "wasm");
  let rec_id = ActorId::scoped("wf", "rec");

  let mut env = std::collections::BTreeMap::new();
  env.insert("component".to_owned(), "echo".to_owned());
  let wasm_cfg = ActorConfig {
    env,
    settings: Default::default(),
  };

  engine
    .add_node(wasm_id.clone(), "wasm", &wasm_cfg, ActorCapabilities::new())
    .await
    .expect("add wasm node");
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
    .add_edge(wasm_id.clone(), rec_id.clone())
    .expect("add edge");

  // Push a message into the wasm node; it echoes onward to the recorder.
  engine
    .push(&wasm_id, Message::json("test", serde_json::json!(42)))
    .expect("push");

  tokio::time::timeout(Duration::from_secs(5), notify.notified())
    .await
    .expect("recorder received an emission");

  let recorded = out.lock().expect("recorder lock");
  assert_eq!(recorded.len(), 1, "expected one emission, got {recorded:?}");

  // The component echoes binary JSON: {"echoed": <value>, "node": "<node-id>"}.
  let MessageValue::Binary(bytes) = &recorded[0].value else {
    panic!("expected binary message, got {:?}", recorded[0].value);
  };
  let v: serde_json::Value = serde_json::from_slice(bytes).expect("valid JSON from component");
  assert_eq!(v["echoed"], serde_json::json!(42));
  assert_eq!(v["node"], serde_json::json!("wf/wasm"));
  assert_eq!(recorded[0].type_, "echo");
}
