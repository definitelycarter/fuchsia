//! The dead-letter sink (part 4) through the engine: a node whose `handle`
//! exhausts its `retry` budget, or stops under `fail`, hands the message to a
//! product-provided `DeadLetter` capability inserted into the node's
//! `ActorCapabilities` — keyed by the triggering run's correlation. The sink is
//! a *domain* capability the product inserts (`caps.insert::<dyn DeadLetter>`),
//! exactly like a real product would, never a fuchsia `with_*` helper.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Backoff,
  FailurePolicy, Message, MessageValue, async_trait,
};
use fuchsia_engine::{CorrelationId, DeadLetter, DeadLetterReason, DeadLettered, Engine};
use tokio::sync::Notify;

// ---- An actor whose `handle` always errors -----------------------------------

struct ErrActor {
  calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Actor for ErrActor {
  async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    self.calls.fetch_add(1, Ordering::SeqCst);
    Err(ActorError::Handle("boom".to_owned()))
  }
}

struct ErrCreator {
  calls: Arc<AtomicUsize>,
}

impl ActorCreator for ErrCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(ErrActor {
      calls: self.calls.clone(),
    }))
  }
}

// ---- A recording dead-letter sink (the product's seam, minus the store) ------

struct RecorderSink {
  letters: Arc<Mutex<Vec<DeadLettered>>>,
  notify: Arc<Notify>,
}

impl DeadLetter for RecorderSink {
  fn dead_letter(&self, letter: DeadLettered) {
    self.letters.lock().unwrap().push(letter);
    self.notify.notify_one();
  }
}

struct SinkHandles {
  letters: Arc<Mutex<Vec<DeadLettered>>>,
  notify: Arc<Notify>,
}

/// Build a recording sink as an `Arc<dyn DeadLetter>` (the shape inserted into
/// the caps bag) plus handles to inspect what it recorded.
fn sink() -> (Arc<dyn DeadLetter>, SinkHandles) {
  let letters = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());
  let sink: Arc<dyn DeadLetter> = Arc::new(RecorderSink {
    letters: letters.clone(),
    notify: notify.clone(),
  });
  (sink, SinkHandles { letters, notify })
}

fn retry_config() -> ActorConfig {
  ActorConfig {
    // 2 retries → 3 attempts before exhaustion; fast fixed backoff for the test.
    failure: FailurePolicy::retry(2, Backoff::fixed(Duration::from_millis(1))),
    ..Default::default()
  }
}

// ---- Tests -------------------------------------------------------------------

#[tokio::test]
async fn exhausted_retry_dead_letters_through_the_engine() {
  let calls = Arc::new(AtomicUsize::new(0));
  let (dl, handles) = sink();

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;

  // The product inserts its dead-letter impl as a domain capability — the
  // generic seam, no dedicated helper — then hands the bag to `add_node`.
  let mut caps = ActorCapabilities::new();
  caps.insert::<dyn DeadLetter>(dl);
  engine
    .add_node(ActorId::new("failing"), "failing", &retry_config(), caps)
    .await
    .unwrap();

  // A known run id so we can assert the dead letter is keyed by it.
  let run = CorrelationId::from("run-xyz");
  engine
    .push(
      &ActorId::new("failing"),
      Message::json("reading", serde_json::json!({ "temp": 99 })),
      run,
    )
    .unwrap();
  handles.notify.notified().await;

  let letters = handles.letters.lock().unwrap();
  assert_eq!(letters.len(), 1);
  let letter = &letters[0];
  assert_eq!(letter.msg.type_, "reading");
  assert_eq!(
    letter.msg.value,
    MessageValue::Json(serde_json::json!({ "temp": 99 }))
  );
  assert_eq!(letter.node, ActorId::new("failing"));
  assert_eq!(letter.correlation.as_str(), "run-xyz");
  assert_eq!(
    letter.reason,
    DeadLetterReason::RetryExhausted {
      attempts: 3,
      error: "handle failed: boom".to_owned(),
    }
  );
  // 1 initial + 2 retries.
  assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn fail_dead_letters_through_the_engine() {
  let calls = Arc::new(AtomicUsize::new(0));
  let (dl, handles) = sink();

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;

  let mut caps = ActorCapabilities::new();
  caps.insert::<dyn DeadLetter>(dl);
  let config = ActorConfig {
    failure: FailurePolicy::fail(),
    ..Default::default()
  };
  engine
    .add_node(ActorId::new("failing"), "failing", &config, caps)
    .await
    .unwrap();

  engine
    .push(
      &ActorId::new("failing"),
      Message::empty("boom"),
      CorrelationId::from("run-fail"),
    )
    .unwrap();
  handles.notify.notified().await;

  let letters = handles.letters.lock().unwrap();
  assert_eq!(letters.len(), 1);
  let letter = &letters[0];
  assert_eq!(letter.msg.type_, "boom");
  assert_eq!(letter.node, ActorId::new("failing"));
  assert_eq!(letter.correlation.as_str(), "run-fail");
  assert_eq!(
    letter.reason,
    DeadLetterReason::Failed {
      error: "handle failed: boom".to_owned(),
    }
  );
  assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn exhausted_retry_with_sink_reports_ok_to_a_durable_caller() {
  // With a sink present, an exhausted retry is the runtime's responsibility, so
  // `push_durable` resolves `Ok` — an at-least-once caller does not retry and
  // produce a *duplicate* dead-letter. (Contrast: `durable.rs` /
  // `route_to_error.rs` assert the no-sink and route_to_error cases.)
  let calls = Arc::new(AtomicUsize::new(0));
  let (dl, handles) = sink();

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;

  let mut caps = ActorCapabilities::new();
  caps.insert::<dyn DeadLetter>(dl);
  engine
    .add_node(ActorId::new("failing"), "failing", &retry_config(), caps)
    .await
    .unwrap();

  engine
    .push_durable(
      &ActorId::new("failing"),
      Message::empty("job"),
      CorrelationId::new(),
    )
    .await
    .unwrap();

  // The durable ack saw Ok, and the message was preserved exactly once.
  assert_eq!(calls.load(Ordering::SeqCst), 3);
  assert_eq!(handles.letters.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn poison_redelivery_is_quarantined_through_push_durable_attempt() {
  // A poison message re-delivered via `push_durable_attempt` with an attempt
  // count over the node's `poison_after` is quarantined to the dead-letter sink
  // (reason Poison) *without* `handle` running, and `push_durable_attempt`
  // resolves `Ok` so the feeder stops re-delivering. End-to-end through the
  // engine: the field stamped by `with_attempts` is the one the runtime gate
  // reads.
  let calls = Arc::new(AtomicUsize::new(0));
  let (dl, handles) = sink();

  let engine = Engine::new();
  engine
    .register(
      "failing",
      ErrCreator {
        calls: calls.clone(),
      },
    )
    .await;

  let mut caps = ActorCapabilities::new();
  caps.insert::<dyn DeadLetter>(dl);
  // `poison_after: 2` — a delivery with attempts > 2 is quarantined.
  let config = ActorConfig {
    failure: FailurePolicy::poison_after(2),
    ..Default::default()
  };
  engine
    .add_node(ActorId::new("failing"), "failing", &config, caps)
    .await
    .unwrap();

  // The feeder re-delivers a previously-Lost message as attempt 3 (> 2).
  engine
    .push_durable_attempt(
      &ActorId::new("failing"),
      Message::empty("job"),
      CorrelationId::from("run-poison"),
      3,
    )
    .await
    .expect("quarantine reports Ok so the feeder stops re-delivering");
  handles.notify.notified().await;

  // Quarantined to the sink (reason Poison { attempts: 3 }), keyed by run.
  let letters = handles.letters.lock().unwrap();
  assert_eq!(letters.len(), 1);
  assert_eq!(letters[0].msg.type_, "job");
  assert_eq!(letters[0].node, ActorId::new("failing"));
  assert_eq!(letters[0].correlation.as_str(), "run-poison");
  assert_eq!(letters[0].reason, DeadLetterReason::Poison { attempts: 3 });
  // `handle` never ran — the always-erroring actor was never invoked.
  assert_eq!(calls.load(Ordering::SeqCst), 0);
}
