//! A mixed-runtime pipeline: a Lua actor, a native builtin, and a Wasm
//! component, all wired into one engine graph and communicating by message.
//!
//! ```text
//!   push ─▶ [lua: convert]  ─▶  [builtin: dedup]  ─▶  [wasm: wrap]  ─▶  [printer]
//!            °C → °F            drop repeated         {"echoed":…,        stdout
//!            (computes)         readings             "node":…}
//! ```
//!
//! Feeding `20, 20, 25`:
//! - **lua** turns each Celsius reading into `{"celsius":C,"fahrenheit":F}`,
//! - **dedup** (a native builtin) drops the second `20` — its value is identical
//!   to the first — so only two messages continue,
//! - **wasm** wraps each surviving message as `{"echoed":…,"node":…}`,
//! - the printer shows the two final messages.
//!
//! Two messages out of three in is the visible proof all three runtimes took
//! part: lua computed, the builtin filtered, wasm wrapped.
//!
//! Run it:
//! ```bash
//! (cd test-components/actor-echo && cargo component build --release)
//! cargo run -p fuchsia-examples
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId,
  COMPONENT_ENV_KEY, Message, MessageValue,
};
use fuchsia_actor_builtins::DedupCreator;
use fuchsia_actor_lua::{BaseLuaHost, LuaActorCreator};
use fuchsia_actor_wasm::{BaseHost, WasmActorCreator};
use fuchsia_engine::Engine;
use tokio::sync::mpsc::{self, UnboundedSender};

const ECHO_WASM: &str = concat!(
  env!("CARGO_MANIFEST_DIR"),
  "/../../test-components/actor-echo/target/wasm32-wasip1/release/actor_echo.wasm"
);

/// Lua stage: parse the Celsius reading and emit a Celsius/Fahrenheit object.
const CONVERT_LUA: &str = r#"
function handle(ctx, msg)
  local celsius = tonumber(msg.value.data)
  local fahrenheit = celsius * 9 / 5 + 32
  emit({
    type = "reading",
    value = {
      kind = "json",
      data = string.format('{"celsius": %g, "fahrenheit": %g}', celsius, fahrenheit)
    }
  })
end
"#;

/// Terminal native actor: forwards every final message to the main task.
struct Printer {
  tx: UnboundedSender<Message>,
}

impl Actor for Printer {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let _ = self.tx.send(msg);
    Ok(())
  }
  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct PrinterCreator {
  tx: UnboundedSender<Message>,
}

impl ActorCreator for PrinterCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Printer {
      tx: self.tx.clone(),
    }))
  }
}

/// An `ActorConfig` whose `env` names the component/script the guest creator
/// should load — exactly what the provisioner writes for a `Component` node.
fn component_config(id: &str) -> ActorConfig {
  let mut env = BTreeMap::new();
  env.insert(COMPONENT_ENV_KEY.to_owned(), id.to_owned());
  ActorConfig {
    env,
    settings: Default::default(),
  }
}

#[tokio::main]
async fn main() {
  if !std::path::Path::new(ECHO_WASM).exists() {
    eprintln!(
      "echo component not built.\n\nBuild it first:\n  (cd test-components/actor-echo && \
       cargo component build --release)\n\nthen re-run:\n  cargo run -p fuchsia-examples"
    );
    return;
  }

  // ── Register the runtimes ────────────────────────────────────────────────
  // One creator per runtime kind; each guest node names its component in env.
  let lua = LuaActorCreator::new(BaseLuaHost::new()).with_source("convert", CONVERT_LUA);
  let wasm = WasmActorCreator::new(BaseHost::new())
    .expect("build wasm creator")
    .with_path("echo", ECHO_WASM)
    .expect("load echo component");

  let (tx, mut rx) = mpsc::unbounded_channel();

  let engine = Engine::new();
  engine.register("lua", lua).await;
  engine.register("wasm", wasm).await;
  engine.register("dedup", DedupCreator).await;
  engine.register("printer", PrinterCreator { tx }).await;

  // ── Provision the graph (group = "demo") ─────────────────────────────────
  let convert = ActorId::scoped("demo", "convert"); // lua
  let dedup = ActorId::scoped("demo", "dedup"); // builtin
  let wrap = ActorId::scoped("demo", "wrap"); // wasm
  let out = ActorId::scoped("demo", "out"); // native printer

  engine
    .add_node(
      convert.clone(),
      "lua",
      &component_config("convert"),
      ActorCapabilities::new(),
    )
    .await
    .expect("add lua node");
  engine
    .add_node(
      dedup.clone(),
      "dedup",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .expect("add dedup node");
  engine
    .add_node(
      wrap.clone(),
      "wasm",
      &component_config("echo"),
      ActorCapabilities::new(),
    )
    .await
    .expect("add wasm node");
  engine
    .add_node(
      out.clone(),
      "printer",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .expect("add printer node");

  engine
    .add_edge(convert.clone(), dedup.clone())
    .expect("edge convert→dedup");
  engine
    .add_edge(dedup.clone(), wrap.clone())
    .expect("edge dedup→wrap");
  engine
    .add_edge(wrap.clone(), out.clone())
    .expect("edge wrap→out");

  // ── Push readings into the entrypoint (the lua node) ─────────────────────
  let readings = [20, 20, 25];
  println!("pushing readings: {readings:?}  (the repeated 20 should be deduped)\n");
  for celsius in readings {
    engine
      .push(
        &convert,
        Message::json("celsius", serde_json::json!(celsius)),
      )
      .expect("push reading");
  }

  // ── Collect what reaches the printer ─────────────────────────────────────
  let mut received = Vec::new();
  // Two readings should survive dedup; wait a moment past that to confirm the
  // duplicate really was dropped (no third message arrives).
  while received.len() < 2 {
    match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
      Ok(Some(msg)) => received.push(msg),
      _ => break,
    }
  }
  let stray = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;

  println!("final messages at the printer: {}\n", received.len());
  for (i, msg) in received.iter().enumerate() {
    match &msg.value {
      MessageValue::Binary(bytes) => {
        println!(
          "  [{i}] type={:<6} {}",
          msg.type_,
          String::from_utf8_lossy(bytes)
        );
      }
      other => println!("  [{i}] type={:<6} {other:?}", msg.type_),
    }
  }

  println!();
  assert_eq!(received.len(), 2, "expected two messages to survive dedup");
  assert!(
    matches!(stray, Err(_) | Ok(None)),
    "dedup should have dropped the repeated reading"
  );
  println!("✓ lua computed °F, the dedup builtin dropped the repeat, and wasm wrapped the rest.");
}
