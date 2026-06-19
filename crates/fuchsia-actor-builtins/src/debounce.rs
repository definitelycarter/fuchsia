use std::sync::Arc;
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, Emit, Message,
  MessageValue, Schedule,
};
use serde::Deserialize;

use crate::from_settings;

/// Self-scheduled timer types. Real input never carries these, so they're safe
/// to distinguish from a value. `FIRE` carries the input `generation`; `MAXWAIT`
/// carries the pending-`period`.
const FIRE: &str = "fuchsia/debounce/fire";
const MAXWAIT: &str = "fuchsia/debounce/maxwait";

#[derive(Debug, Deserialize)]
struct DebounceConfig {
  /// Quiet window in milliseconds: emit the latest value once no newer input
  /// has arrived for this long.
  delay_ms: u64,
  /// Optional upper bound: emit the latest value at least this often even if
  /// input never goes quiet. Without it, a never-quiet stream (e.g. a sensor
  /// reporting faster than `delay_ms`) would starve — the timer re-arms forever
  /// and nothing is emitted.
  #[serde(default)]
  max_wait_ms: Option<u64>,
}

/// Trailing-edge debounce: holds the most recent value and emits it once input
/// has been quiet for `delay`. Each input re-arms the quiet timer.
///
/// Re-arming is cancellation-free (the [`Schedule`] capability is fire-and-
/// forget): every input bumps `generation` and schedules a `FIRE` tagged with
/// it; a timer left stale by a newer input sees a mismatch and drops.
///
/// `max_wait` guards against starvation on a never-quiet stream. At the *start*
/// of a pending period (first input since the last emit) a single `MAXWAIT`
/// timer is anchored — not re-armed per input — so the latest value is emitted
/// at least every `max_wait` even if the quiet window never elapses. `period`
/// tags it the same way `generation` tags `FIRE`.
pub struct Debounce {
  emit: Arc<dyn Emit>,
  schedule: Arc<dyn Schedule>,
  delay: Duration,
  max_wait: Option<Duration>,
  latest: Option<Message>,
  generation: u64,
  period: u64,
}

impl Actor for Debounce {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }

  fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    match msg.type_.as_str() {
      // Quiet window elapsed — emit unless a newer input re-armed since.
      FIRE if tagged(&msg) == Some(self.generation) => self.flush(),
      // Upper bound elapsed — emit the latest even if never quiet.
      MAXWAIT if tagged(&msg) == Some(self.period) => self.flush(),
      FIRE | MAXWAIT => {} // stale timer, drop
      _ => {
        let starting = self.latest.is_none();
        self.generation = self.generation.wrapping_add(1);
        self.latest = Some(msg);
        self
          .schedule
          .schedule_self(self.delay, tag(FIRE, self.generation));
        // Anchor one max-wait timer per pending period, at its start.
        if let Some(max_wait) = self.max_wait {
          if starting {
            self.period = self.period.wrapping_add(1);
            self
              .schedule
              .schedule_self(max_wait, tag(MAXWAIT, self.period));
          }
        }
      }
    }
    Ok(())
  }

  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

impl Debounce {
  /// Emit the held value, ending the pending period. Idempotent: a second timer
  /// firing after a flush finds `latest` empty and does nothing.
  fn flush(&mut self) {
    if let Some(latest) = self.latest.take() {
      self.emit.emit(latest);
    }
  }
}

fn tag(type_: &str, counter: u64) -> Message {
  Message::json(type_, serde_json::Value::from(counter))
}

fn tagged(msg: &Message) -> Option<u64> {
  match &msg.value {
    MessageValue::Json(v) => v.as_u64(),
    _ => None,
  }
}

pub struct DebounceCreator;

impl ActorCreator for DebounceCreator {
  fn create(
    &self,
    config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    let cfg: DebounceConfig = from_settings(&config.settings)?;
    Ok(Box::new(Debounce {
      emit: caps.emit(),
      schedule: caps.schedule(),
      delay: Duration::from_millis(cfg.delay_ms),
      max_wait: cfg.max_wait_ms.map(Duration::from_millis),
      latest: None,
      generation: 0,
      period: 0,
    }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use bson::{Document, doc};
  use std::sync::Mutex;

  /// Records emitted messages.
  struct Capture(Arc<Mutex<Vec<Message>>>);
  impl Emit for Capture {
    fn emit(&self, msg: Message) {
      self.0.lock().unwrap().push(msg);
    }
  }

  /// Records the messages an actor schedules to itself, so a test can replay
  /// them through `handle` deterministically (no real timers).
  struct FakeSchedule(Arc<Mutex<Vec<Message>>>);
  impl Schedule for FakeSchedule {
    fn schedule_self(&self, _after: Duration, msg: Message) {
      self.0.lock().unwrap().push(msg);
    }
  }

  fn make(
    settings: Document,
  ) -> (
    Box<dyn Actor>,
    Arc<Mutex<Vec<Message>>>,
    Arc<Mutex<Vec<Message>>>,
  ) {
    let emitted = Arc::new(Mutex::new(Vec::new()));
    let timers = Arc::new(Mutex::new(Vec::new()));
    let caps = ActorCapabilities::new()
      .with_emit(Arc::new(Capture(emitted.clone())))
      .with_schedule(Arc::new(FakeSchedule(timers.clone())));
    let config = ActorConfig {
      settings,
      ..Default::default()
    };
    let actor = DebounceCreator.create(&config, &caps).unwrap();
    (actor, emitted, timers)
  }

  fn build(
    delay_ms: i64,
  ) -> (
    Box<dyn Actor>,
    Arc<Mutex<Vec<Message>>>,
    Arc<Mutex<Vec<Message>>>,
  ) {
    make(doc! { "delay_ms": delay_ms })
  }

  fn ctx() -> ActorContext {
    ActorContext::new("e", "n", "t")
  }

  #[test]
  fn missing_delay_is_a_config_error() {
    let caps = ActorCapabilities::new();
    let err = DebounceCreator
      .create(&ActorConfig::default(), &caps)
      .err()
      .unwrap();
    assert!(matches!(err, ActorError::Config(_)));
  }

  #[test]
  fn emits_latest_after_quiet_window() {
    let (mut actor, emitted, timers) = build(50);

    actor.handle(&ctx(), Message::empty("reading")).unwrap();
    assert!(emitted.lock().unwrap().is_empty());
    assert_eq!(timers.lock().unwrap().len(), 1);

    let fire = timers.lock().unwrap()[0].clone();
    actor.handle(&ctx(), fire).unwrap();
    let out = emitted.lock().unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].type_, "reading");
  }

  #[test]
  fn newer_input_invalidates_an_older_timer() {
    let (mut actor, emitted, timers) = build(50);

    actor.handle(&ctx(), Message::json("a", 1.into())).unwrap();
    actor.handle(&ctx(), Message::json("b", 2.into())).unwrap();
    assert_eq!(timers.lock().unwrap().len(), 2);

    let gen1 = timers.lock().unwrap()[0].clone();
    let gen2 = timers.lock().unwrap()[1].clone();

    actor.handle(&ctx(), gen1).unwrap();
    assert!(emitted.lock().unwrap().is_empty());

    actor.handle(&ctx(), gen2).unwrap();
    let out = emitted.lock().unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].type_, "b");
  }

  #[test]
  fn max_wait_emits_under_never_quiet_input() {
    let (mut actor, emitted, timers) = make(doc! { "delay_ms": 50_i64, "max_wait_ms": 200_i64 });

    // Two inputs, and we never replay a FIRE — simulating a stream that never
    // goes quiet, where debounce alone would starve.
    actor.handle(&ctx(), Message::json("a", 1.into())).unwrap();
    actor.handle(&ctx(), Message::json("b", 2.into())).unwrap();
    assert!(emitted.lock().unwrap().is_empty());

    let scheduled = timers.lock().unwrap().clone();
    // One max-wait timer, anchored at the period start (not re-armed per input).
    assert_eq!(scheduled.iter().filter(|m| m.type_ == MAXWAIT).count(), 1);

    // It fires and emits the latest value despite no quiet gap.
    let maxwait = scheduled
      .iter()
      .find(|m| m.type_ == MAXWAIT)
      .unwrap()
      .clone();
    actor.handle(&ctx(), maxwait).unwrap();
    assert_eq!(emitted.lock().unwrap().len(), 1);
    assert_eq!(emitted.lock().unwrap()[0].type_, "b");

    // The still-pending FIRE for the last input fires later but finds nothing
    // to flush — no double emit.
    let fire = scheduled
      .iter()
      .rev()
      .find(|m| m.type_ == FIRE)
      .unwrap()
      .clone();
    actor.handle(&ctx(), fire).unwrap();
    assert_eq!(emitted.lock().unwrap().len(), 1);
  }
}
