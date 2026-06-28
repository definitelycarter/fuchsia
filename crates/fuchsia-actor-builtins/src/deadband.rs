use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, Emit, Message,
  MessageValue, async_trait,
};
use serde::Deserialize;

use crate::from_settings;

#[derive(Debug, Deserialize)]
struct DeadbandConfig {
  /// Minimum change from the last emitted value required to emit again.
  threshold: f64,
}

/// Suppresses changes below `threshold`: emits a numeric reading only when it
/// differs from the last *emitted* value by at least the threshold. Comparing
/// against the last emitted value (not the last seen) keeps a slow drift from
/// accumulating silently — each emission resets the reference point.
///
/// Non-numeric messages pass through untouched — deadband only reasons about
/// numbers.
pub struct Deadband {
  emit: Arc<dyn Emit>,
  threshold: f64,
  last_emitted: Option<f64>,
}

#[async_trait]
impl Actor for Deadband {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }

  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    match numeric(&msg) {
      Some(value) => {
        let passes = match self.last_emitted {
          None => true,
          Some(last) => (value - last).abs() >= self.threshold,
        };
        if passes {
          self.last_emitted = Some(value);
          self.emit.emit(msg);
        }
      }
      // Can't deadband a non-number; let it through.
      None => self.emit.emit(msg),
    }
    Ok(())
  }

  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

fn numeric(msg: &Message) -> Option<f64> {
  match &msg.value {
    MessageValue::Json(v) => v.as_f64(),
    _ => None,
  }
}

pub struct DeadbandCreator;

impl ActorCreator for DeadbandCreator {
  fn create(
    &self,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    let cfg: DeadbandConfig = from_settings(&config.settings)?;
    Ok(Box::new(Deadband {
      emit: caps.emit(),
      threshold: cfg.threshold,
      last_emitted: None,
    }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use bson::doc;
  use std::sync::Mutex;

  struct Capture(Arc<Mutex<Vec<Message>>>);
  impl Emit for Capture {
    fn emit(&self, msg: Message) {
      self.0.lock().unwrap().push(msg);
    }
  }

  fn build(threshold: f64) -> (Box<dyn Actor>, Arc<Mutex<Vec<Message>>>) {
    let emitted = Arc::new(Mutex::new(Vec::new()));
    let caps = ActorCapabilities::new().with_emit(Arc::new(Capture(emitted.clone())));
    let config = ActorConfig {
      settings: doc! { "threshold": threshold },
      ..Default::default()
    };
    let actor = DeadbandCreator.create(&config, &caps).unwrap();
    (actor, emitted)
  }

  fn reading(v: f64) -> Message {
    Message::json("reading", v.into())
  }

  fn ctx() -> ActorContext {
    ActorContext::new("e", "n", "t")
  }

  #[tokio::test]
  async fn emits_only_on_changes_at_or_above_threshold() {
    let (mut actor, emitted) = build(1.0);

    actor.handle(&ctx(), reading(10.0)).await.unwrap(); // first: emits
    actor.handle(&ctx(), reading(10.5)).await.unwrap(); // +0.5 < 1.0: suppressed
    actor.handle(&ctx(), reading(11.0)).await.unwrap(); // +1.0 from 10.0: emits
    actor.handle(&ctx(), reading(11.4)).await.unwrap(); // +0.4 from 11.0: suppressed

    let out = emitted.lock().unwrap();
    let values: Vec<f64> = out.iter().filter_map(numeric).collect();
    assert_eq!(values, vec![10.0, 11.0]);
  }

  #[tokio::test]
  async fn non_numeric_passes_through() {
    let (mut actor, emitted) = build(1.0);
    actor.handle(&ctx(), Message::empty("ping")).await.unwrap();
    assert_eq!(emitted.lock().unwrap().len(), 1);
  }
}
