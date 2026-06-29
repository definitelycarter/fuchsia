use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, Emit, Message,
  async_trait,
};

/// Forwards every message through unchanged. Useful for wiring up and debugging
/// a graph before real operators exist — it's the simplest possible node that
/// still exercises the full receive → emit path.
pub struct Passthrough {
  emit: Arc<dyn Emit>,
}

#[async_trait]
impl Actor for Passthrough {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }

  async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.emit.emit(msg);
    Ok(())
  }

  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

pub struct PassthroughCreator;

impl ActorCreator for PassthroughCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Passthrough { emit: caps.emit() }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use fuchsia_actor::MessageValue;
  use std::sync::Mutex;

  /// Test sink that records everything emitted.
  struct Capture(Arc<Mutex<Vec<Message>>>);

  impl Emit for Capture {
    fn emit_to(&self, _port: &str, msg: Message) {
      self.0.lock().unwrap().push(msg);
    }
  }

  #[tokio::test]
  async fn emits_input_unchanged() {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let caps = ActorCapabilities::new().with_emit(Arc::new(Capture(sink.clone())));

    let mut actor = PassthroughCreator
      .create(&ActorConfig::default(), &caps)
      .unwrap();
    let ctx = ActorContext::new("exec", "node", "task");
    actor.handle(&ctx, Message::empty("reading")).await.unwrap();

    let out = sink.lock().unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].type_, "reading");
    assert!(matches!(out[0].value, MessageValue::Empty));
  }
}
