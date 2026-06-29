use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, Emit, Message,
  MessageValue, async_trait,
};

/// Drops consecutive duplicate values: emits a message only when its payload
/// value differs from the previously emitted one. Compares the value, not the
/// message type — the type is the event discriminator and is constant for a
/// given stream; the value is what "changed or not" is about.
///
/// Takes no settings.
pub struct Dedup {
  emit: Arc<dyn Emit>,
  last: Option<MessageValue>,
}

#[async_trait]
impl Actor for Dedup {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }

  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    if self.last.as_ref() != Some(&msg.value) {
      // Retain a copy to compare against the next message before handing the
      // original downstream. Only clones on an actual change, never on a dup.
      self.last = Some(msg.value.clone());
      self.emit.emit(msg);
    }
    Ok(())
  }

  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

pub struct DedupCreator;

impl ActorCreator for DedupCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Dedup {
      emit: caps.emit(),
      last: None,
    }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  struct Capture(Arc<Mutex<Vec<Message>>>);
  impl Emit for Capture {
    fn emit_to(&self, _port: &str, msg: Message) {
      self.0.lock().unwrap().push(msg);
    }
  }

  fn build() -> (Box<dyn Actor>, Arc<Mutex<Vec<Message>>>) {
    let emitted = Arc::new(Mutex::new(Vec::new()));
    let caps = ActorCapabilities::new().with_emit(Arc::new(Capture(emitted.clone())));
    let actor = DedupCreator.create(&ActorConfig::default(), &caps).unwrap();
    (actor, emitted)
  }

  fn ctx() -> ActorContext {
    ActorContext::new("e", "n", "t")
  }

  #[tokio::test]
  async fn drops_consecutive_duplicates() {
    let (mut actor, emitted) = build();

    actor
      .handle(&ctx(), Message::json("r", 1.into()))
      .await
      .unwrap(); // emit
    actor
      .handle(&ctx(), Message::json("r", 1.into()))
      .await
      .unwrap(); // dup: drop
    actor
      .handle(&ctx(), Message::json("r", 2.into()))
      .await
      .unwrap(); // emit
    actor
      .handle(&ctx(), Message::json("r", 2.into()))
      .await
      .unwrap(); // dup: drop
    actor
      .handle(&ctx(), Message::json("r", 1.into()))
      .await
      .unwrap(); // emit (changed)

    let out = emitted.lock().unwrap();
    let values: Vec<i64> = out
      .iter()
      .filter_map(|m| match &m.value {
        MessageValue::Json(v) => v.as_i64(),
        _ => None,
      })
      .collect();
    assert_eq!(values, vec![1, 2, 1]);
  }
}
