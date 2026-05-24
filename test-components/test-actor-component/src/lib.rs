wit_bindgen::generate!({
    path: "../../wit",
    world: "actor-component",
    generate_all,
});

struct TestActor;

export!(TestActor);

impl exports::fuchsia::actor::actor::Guest for TestActor {
  fn setup(ctx: exports::fuchsia::actor::actor::Context) -> Result<(), String> {
    fuchsia::log::log::log(
      fuchsia::log::log::Level::Info,
      &format!("test-actor-component: setup node {}", ctx.node_id),
    );
    Ok(())
  }

  fn handle(
    ctx: exports::fuchsia::actor::actor::Context,
    msg: fuchsia::actor::types::Payload,
  ) -> Result<(), String> {
    fuchsia::log::log::log(
      fuchsia::log::log::Level::Info,
      &format!(
        "test-actor-component: handle node {} type {}",
        ctx.node_id, msg.type_
      ),
    );

    let echoed_data = match &msg.value {
      fuchsia::actor::types::PayloadValue::Json(s) => s.clone(),
      fuchsia::actor::types::PayloadValue::Binary(_) => "\"binary\"".to_string(),
      fuchsia::actor::types::PayloadValue::Empty => "null".to_string(),
    };

    let out_json = format!(
      r#"{{"echoed": {}, "node": "{}"}}"#,
      echoed_data, ctx.node_id
    );

    fuchsia::actor::emit::send(&fuchsia::actor::types::Payload {
      type_: "echo".to_string(),
      value: fuchsia::actor::types::PayloadValue::Json(out_json),
    })
  }

  fn teardown(ctx: exports::fuchsia::actor::actor::Context) -> Result<(), String> {
    fuchsia::log::log::log(
      fuchsia::log::log::Level::Info,
      &format!("test-actor-component: teardown node {}", ctx.node_id),
    );
    Ok(())
  }
}
