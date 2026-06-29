//! The `if` builtin: a predicate over the payload that routes each message to
//! the `"true"` or `"false"` output port. The predicate is *configuration* (a
//! [`Condition`] in the node's `settings`), not code — the actor is a generic
//! evaluator written once.

use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, Emit, Message,
  OutputPorts, async_trait,
};
use serde::Deserialize;

use crate::condition::{Condition, PreparedCondition};
use crate::from_settings;

/// The two ports an `if` node always has.
const TRUE_PORT: &str = "true";
const FALSE_PORT: &str = "false";

/// An `if` node's config: just the condition to evaluate. The condition
/// document is the whole `settings` body — `{ "field": …, "op": …, "value": … }`
/// or `{ "expr": … }`.
#[derive(Debug, Deserialize)]
struct IfConfig {
  #[serde(flatten)]
  condition: Condition,
}

/// Routes each input to `"true"` or `"false"` by evaluating a [`Condition`]
/// over its payload. The message itself is forwarded unchanged on the chosen
/// port — the branch is *where* it goes, not a transformation.
pub struct If {
  emit: Arc<dyn Emit>,
  // Prepared once at construction (the `expr` arm's minijinja env is built and
  // its syntax validated there), so `handle` only evaluates.
  condition: PreparedCondition,
}

#[async_trait]
impl Actor for If {
  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let port = if self.condition.evaluate(&msg)? {
      TRUE_PORT
    } else {
      FALSE_PORT
    };
    self.emit.emit_to(port, msg);
    Ok(())
  }
}

pub struct IfCreator;

impl ActorCreator for IfCreator {
  fn create(
    &self,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    let cfg: IfConfig = from_settings(&config.settings)?;
    Ok(Box::new(If {
      emit: caps.emit(),
      // Validate + pre-build now, so a malformed `expr` fails at provision time.
      condition: cfg.condition.prepare()?,
    }))
  }

  fn output_ports(&self, _config: &ActorConfig) -> OutputPorts {
    // Fixed by the type — an `if` always has exactly these two.
    OutputPorts::Fixed(vec![TRUE_PORT.to_owned(), FALSE_PORT.to_owned()])
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use bson::{Document, doc};
  use std::sync::Mutex;

  /// Records `(port, message)` for every emission, so a test can assert *which*
  /// port a message left on.
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
    let actor = IfCreator.create(&config, &caps).unwrap();
    (actor, emitted)
  }

  fn ctx() -> ActorContext {
    ActorContext::new("e", "n", 1)
  }

  #[test]
  fn declares_true_false_ports() {
    let ports = IfCreator.output_ports(&ActorConfig::default());
    assert_eq!(
      ports,
      OutputPorts::Fixed(vec!["true".to_owned(), "false".to_owned()])
    );
  }

  #[tokio::test]
  async fn routes_to_true_when_predicate_holds() {
    let (mut actor, emitted) = make(doc! { "field": "temp", "op": "gt", "value": 30 });
    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "temp": 42 })),
      )
      .await
      .unwrap();
    let out = emitted.lock().unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "true");
  }

  #[tokio::test]
  async fn routes_to_false_when_predicate_fails() {
    let (mut actor, emitted) = make(doc! { "field": "temp", "op": "gt", "value": 30 });
    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "temp": 20 })),
      )
      .await
      .unwrap();
    let out = emitted.lock().unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "false");
  }

  #[tokio::test]
  async fn all_group_routes_true_only_when_every_arm_holds() {
    let (mut actor, emitted) = make(doc! { "all": [
      { "field": "temp", "op": "gt", "value": 30 },
      { "field": "humidity", "op": "lt", "value": 50 },
    ] });
    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "temp": 42, "humidity": 40 })),
      )
      .await
      .unwrap();
    assert_eq!(emitted.lock().unwrap()[0].0, "true");

    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "temp": 42, "humidity": 60 })),
      )
      .await
      .unwrap();
    assert_eq!(emitted.lock().unwrap()[1].0, "false");
  }

  #[tokio::test]
  async fn expr_arm_routes_like_a_predicate() {
    // The minijinja expr arm evaluates over the payload and branches the same
    // way the declarative arm does.
    let (mut actor, emitted) = make(doc! { "expr": "temp > 30" });
    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "temp": 42 })),
      )
      .await
      .unwrap();
    assert_eq!(emitted.lock().unwrap()[0].0, "true");

    actor
      .handle(
        &ctx(),
        Message::json("reading", serde_json::json!({ "temp": 20 })),
      )
      .await
      .unwrap();
    assert_eq!(emitted.lock().unwrap()[1].0, "false");
  }

  #[test]
  fn invalid_expr_surfaces_as_a_config_error_at_construction() {
    // The expression's syntax is validated once when the node is built, so a
    // malformed `expr` fails at `create` (provision time), not per message.
    let config = ActorConfig {
      settings: doc! { "expr": "temp >" },
      ..Default::default()
    };
    let err = IfCreator
      .create(&config, &ActorCapabilities::new())
      .err()
      .unwrap();
    assert!(matches!(err, ActorError::Config(_)));
  }
}
