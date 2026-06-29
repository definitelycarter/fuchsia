//! The `switch` builtin: extract a key from the payload and route each message
//! to the matching case port, falling back to `"default"`. The cases are
//! *configuration* — listing them in `settings` is what configures the node's
//! ports (`cases` + `default`).

use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, Emit, Message,
  MessageValue, OutputPorts, async_trait,
};
use serde::Deserialize;

use crate::from_settings;

/// The fallback port for a key that matches no configured case.
const DEFAULT_PORT: &str = "default";

/// A `switch` node's config: the payload field to extract (`key`) and the case
/// values to match against. The matched case's value *is* the port name.
#[derive(Debug, Clone, Deserialize)]
struct SwitchConfig {
  /// The payload field whose value selects the case port.
  key: String,
  /// The case values — each is also a port name. A value not in this list
  /// routes to `"default"`.
  cases: Vec<String>,
}

/// Routes each input to the port named by its `key` field's value, when that
/// value is one of the configured `cases`; otherwise to `"default"`. The
/// message is forwarded unchanged on the chosen port.
pub struct Switch {
  emit: Arc<dyn Emit>,
  key: String,
  cases: Vec<String>,
}

impl Switch {
  /// The port to route `msg` on: the matching case, or `"default"`. The key's
  /// value is compared as a string (a JSON string by its contents, a number by
  /// its rendering) so case labels stay plain strings.
  fn port_for<'a>(&'a self, msg: &Message) -> &'a str {
    use std::borrow::Cow;

    let MessageValue::Json(payload) = &msg.value else {
      return DEFAULT_PORT;
    };
    let Some(value) = payload.get(&self.key) else {
      return DEFAULT_PORT;
    };
    // Borrow a string value (no allocation on the common path); render any
    // other scalar to an owned string only when needed.
    let key: Cow<str> = match value {
      serde_json::Value::String(s) => Cow::Borrowed(s.as_str()),
      other => Cow::Owned(other.to_string()),
    };
    match self.cases.iter().find(|c| c.as_str() == key) {
      Some(case) => case,
      None => DEFAULT_PORT,
    }
  }
}

#[async_trait]
impl Actor for Switch {
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let port = self.port_for(&msg).to_owned();
    self.emit.emit_to(&port, msg);
    Ok(())
  }
}

pub struct SwitchCreator;

impl SwitchCreator {
  /// The declared ports for a given config: the configured cases plus
  /// `"default"`. Shared by `create`-time validation and `output_ports`.
  fn ports_from(config: &ActorConfig) -> Result<Vec<String>, ActorError> {
    let cfg: SwitchConfig = from_settings(&config.settings)?;
    let mut ports = cfg.cases;
    ports.push(DEFAULT_PORT.to_owned());
    Ok(ports)
  }
}

impl ActorCreator for SwitchCreator {
  fn create(
    &self,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    let cfg: SwitchConfig = from_settings(&config.settings)?;
    Ok(Box::new(Switch {
      emit: caps.emit(),
      key: cfg.key,
      cases: cfg.cases,
    }))
  }

  fn output_ports(&self, config: &ActorConfig) -> OutputPorts {
    // Derived from config — the cases plus `default`. A malformed config can't
    // produce a sensible port set; fall back to just `default` (the `create`
    // call surfaces the real config error). The engine still validates edges
    // against whatever this returns.
    match Self::ports_from(config) {
      Ok(ports) => OutputPorts::Fixed(ports),
      Err(_) => OutputPorts::Fixed(vec![DEFAULT_PORT.to_owned()]),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use bson::{Document, doc};
  use std::sync::Mutex;

  struct Capture(Arc<Mutex<Vec<(String, Message)>>>);
  impl Emit for Capture {
    fn emit_to(&self, port: &str, msg: Message) {
      self.0.lock().unwrap().push((port.to_owned(), msg));
    }
  }

  type Harness = (Box<dyn Actor>, Arc<Mutex<Vec<(String, Message)>>>);

  fn make(settings: Document) -> Harness {
    let emitted = Arc::new(Mutex::new(Vec::new()));
    let caps = ActorCapabilities::new().with_emit(Arc::new(Capture(emitted.clone())));
    let config = ActorConfig {
      settings,
      ..Default::default()
    };
    let actor = SwitchCreator.create(&config, &caps).unwrap();
    (actor, emitted)
  }

  fn settings() -> Document {
    doc! { "key": "kind", "cases": ["temp", "humidity"] }
  }

  fn ctx() -> ActorContext {
    ActorContext::new("e", "n", 1)
  }

  #[test]
  fn declares_cases_plus_default() {
    let config = ActorConfig {
      settings: settings(),
      ..Default::default()
    };
    assert_eq!(
      SwitchCreator.output_ports(&config),
      OutputPorts::Fixed(vec![
        "temp".to_owned(),
        "humidity".to_owned(),
        "default".to_owned()
      ])
    );
  }

  #[test]
  fn missing_config_is_a_config_error() {
    let err = SwitchCreator
      .create(&ActorConfig::default(), &ActorCapabilities::new())
      .err()
      .unwrap();
    assert!(matches!(err, ActorError::Config(_)));
  }

  #[tokio::test]
  async fn routes_to_matching_case() {
    let (mut actor, emitted) = make(settings());
    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "kind": "temp" })),
      )
      .await
      .unwrap();
    assert_eq!(emitted.lock().unwrap()[0].0, "temp");
  }

  #[tokio::test]
  async fn routes_to_default_when_no_case_matches() {
    let (mut actor, emitted) = make(settings());
    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "kind": "pressure" })),
      )
      .await
      .unwrap();
    assert_eq!(emitted.lock().unwrap()[0].0, "default");
  }

  #[tokio::test]
  async fn missing_key_routes_to_default() {
    let (mut actor, emitted) = make(settings());
    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "other": "x" })),
      )
      .await
      .unwrap();
    assert_eq!(emitted.lock().unwrap()[0].0, "default");
  }
}
