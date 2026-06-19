use std::sync::Arc;

use bson::Bson;
use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, Message,
  MessageValue, StateSink,
};

/// Terminal node of an entity's pre-write pipeline: commits a conditioned
/// reading to entity state. This is the boundary — everything upstream
/// (debounce/deadband/dedup) is best-effort and lossy; the commit is the
/// durable write that post-write automation hangs off.
///
/// Holds a [`StateSink`] the host pre-scoped to the entity's storage. The actor
/// writes and never learns where the value lands — same neighbor-ignorance as
/// `emit`, which is what keeps partitioning a host concern.
pub struct Commit {
  sink: Arc<dyn StateSink>,
}

impl Actor for Commit {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }

  fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.sink.write(to_bson(msg.value)?)
  }

  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

/// Serialize a message payload into the `Bson` the sink stores. Scalar or
/// document both pass through (the sink wraps it in the state record).
fn to_bson(value: MessageValue) -> Result<Bson, ActorError> {
  match value {
    MessageValue::Json(v) => {
      bson::serialize_to_bson(&v).map_err(|e| ActorError::StateWrite(e.to_string()))
    }
    MessageValue::Binary(bytes) => Ok(Bson::Binary(bson::Binary {
      subtype: bson::spec::BinarySubtype::Generic,
      bytes,
    })),
    MessageValue::Empty => Ok(Bson::Null),
  }
}

pub struct CommitCreator;

impl ActorCreator for CommitCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    let sink = caps
      .state()
      .ok_or_else(|| ActorError::Config("commit requires a state sink".to_owned()))?;
    Ok(Box::new(Commit { sink }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  /// Records everything written, so a test can assert the committed value
  /// without a real database.
  struct FakeSink(Arc<Mutex<Vec<Bson>>>);
  impl StateSink for FakeSink {
    fn write(&self, value: Bson) -> Result<(), ActorError> {
      self.0.lock().unwrap().push(value);
      Ok(())
    }
  }

  fn ctx() -> ActorContext {
    ActorContext::new("e", "n", "t")
  }

  #[test]
  fn commits_the_message_value_to_the_sink() {
    let written = Arc::new(Mutex::new(Vec::new()));
    let caps = ActorCapabilities::new().with_state(Arc::new(FakeSink(written.clone())));

    let mut actor = CommitCreator
      .create(&ActorConfig::default(), &caps)
      .unwrap();
    actor
      .handle(&ctx(), Message::json("reading", 21.5.into()))
      .unwrap();

    let out = written.lock().unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0], Bson::Double(21.5));
  }

  #[test]
  fn missing_sink_is_a_config_error() {
    let err = CommitCreator
      .create(&ActorConfig::default(), &ActorCapabilities::new())
      .err()
      .unwrap();
    assert!(matches!(err, ActorError::Config(_)));
  }
}
