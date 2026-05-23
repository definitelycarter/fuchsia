wit_bindgen::generate!({
    path: "../../wit",
    world: "task-component",
    generate_all,
});

struct TestTask;

export!(TestTask);

impl exports::fuchsia::task::task::Guest for TestTask {
  fn execute(
    ctx: exports::fuchsia::task::task::Context,
    data: String,
  ) -> Result<exports::fuchsia::task::task::Output, String> {
    // Use host imports to demonstrate they work
    fuchsia::log::log::log(
      fuchsia::log::log::Level::Info,
      &format!(
        "Executing task {} in node {} (execution: {})",
        ctx.task_id, ctx.node_id, ctx.execution_id
      ),
    );

    // Test KV store
    fuchsia::kv::kv::set("last_input", &data);
    let stored = fuchsia::kv::kv::get("last_input");

    // Test config
    let config_val = fuchsia::config::config::get("test_key");

    // Echo back the input with some metadata
    let config_json = match config_val {
      Some(v) => format!(r#""{}""#, v),
      None => "null".to_string(),
    };
    let output = format!(
      r#"{{"input": {}, "stored_matches": {}, "config_value": {}}}"#,
      data,
      stored.as_deref() == Some(&data),
      config_json
    );

    Ok(exports::fuchsia::task::task::Output { data: output })
  }
}
