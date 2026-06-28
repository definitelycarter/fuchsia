//! A Lua script that calls an **async host import**.
//!
//! The host registers an async global via mlua's `create_async_function`; the
//! script calls it like any function; the `LuaActor`'s async `handle` drives the
//! coroutine (mlua `call_async`), so awaiting the import yields the runtime
//! thread instead of blocking it. This exercises the guest → async-host-import
//! path that the contract-only `BaseLuaHost` can't — on a multi-thread runtime,
//! where the actor task may also migrate worker threads across the `.await`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  ActorCapabilities, ActorConfig, ActorContext, ActorCreator, COMPONENT_ENV_KEY, Emit, Message,
  MessageValue,
};
use fuchsia_actor_lua::{LuaActorCreator, LuaHost, mlua};

/// Records everything the actor emits.
struct Capture(Arc<Mutex<Vec<Message>>>);

impl Emit for Capture {
  fn emit(&self, msg: Message) {
    self.0.lock().unwrap().push(msg);
  }
}

/// A `LuaHost` that registers `emit(n)` plus an async `slow_double(n)` global —
/// a stand-in for an async I/O capability (sleep, then compute).
struct AsyncLuaHost;

impl LuaHost for AsyncLuaHost {
  fn populate(&self, lua: &mlua::Lua, emit: Arc<dyn Emit>) -> mlua::Result<()> {
    let emit_fn = lua.create_function(move |_, n: f64| {
      emit.emit(Message::json("result", serde_json::json!(n)));
      Ok(())
    })?;
    lua.globals().set("emit", emit_fn)?;

    let slow_double = lua.create_async_function(|_, n: f64| async move {
      tokio::time::sleep(Duration::from_millis(20)).await;
      Ok(n * 2.0)
    })?;
    lua.globals().set("slow_double", slow_double)?;
    Ok(())
  }
}

fn config(script: &str) -> ActorConfig {
  let mut env = BTreeMap::new();
  env.insert(COMPONENT_ENV_KEY.to_owned(), script.to_owned());
  ActorConfig {
    env,
    settings: Default::default(),
  }
}

fn ctx() -> ActorContext {
  ActorContext::new("exec", "node", "task")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lua_script_awaits_an_async_host_import() {
  let sink = Arc::new(Mutex::new(Vec::new()));
  let creator = LuaActorCreator::new(AsyncLuaHost).with_source(
    "doubler",
    r#"
      function handle(ctx, msg)
        local r = slow_double(21)   -- awaits the async host import
        emit(r)
      end
    "#,
  );

  let caps = ActorCapabilities::new().with_emit(Arc::new(Capture(sink.clone())));
  let mut actor = creator.create(&config("doubler"), &caps).unwrap();
  actor.setup(&ctx()).await.unwrap();
  actor.handle(&ctx(), Message::empty("go")).await.unwrap();

  let out = sink.lock().unwrap();
  assert_eq!(out.len(), 1, "the script should emit exactly once");
  match &out[0].value {
    MessageValue::Json(v) => assert_eq!(v.as_f64(), Some(42.0), "slow_double(21) should be 42"),
    other => panic!("expected json 42.0, got {other:?}"),
  }
}
