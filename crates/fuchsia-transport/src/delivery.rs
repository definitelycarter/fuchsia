use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fuchsia_actor::{ActorError, Message};
use tokio::sync::oneshot;
use tracing::Span;

use crate::correlation::CorrelationId;

/// The outcome of handling one message. Reported exactly once per delivery.
pub type Outcome = Result<(), ActorError>;

/// Health counters for a mailbox / pipeline.
///
/// Pre-write producers report outcomes here so failure is *observable*: a
/// stalled pipeline shows up as a rising error count (and, later, staleness)
/// rather than a silently frozen value.
///
/// `handled`/`errored` are *per-message* outcomes. `died` is a separate,
/// *node-lifecycle* count: the node's task exited unexpectedly (a panic, or an
/// abnormal loop exit) rather than handling another message — a node *death*,
/// not a failed delivery. Keeping it distinct means a crashed node reads as a
/// death, not as one more errored message folded into `errored`.
///
/// `poisoned` is a third, distinct outcome: a delivery whose cross-delivery
/// attempt count crossed the node's `poison_after` threshold and was
/// **quarantined** — diverted without handling so it can't crash the node again
/// (a poison message). Like `died`, it is kept off `errored`/`handled`: a
/// quarantine is neither a normal handle nor a per-message error. It is bumped
/// only on the *fallback* path where no dead-letter sink absorbed the poison
/// (the sink path records it on the sink instead), so a rising `poisoned` count
/// surfaces poison drops that nothing else captured.
#[derive(Debug, Default)]
pub struct Health {
  handled: AtomicU64,
  errored: AtomicU64,
  died: AtomicU64,
  poisoned: AtomicU64,
}

impl Health {
  /// Record one handled outcome.
  pub fn record(&self, outcome: &Outcome) {
    self.handled.fetch_add(1, Ordering::Relaxed);
    if outcome.is_err() {
      self.errored.fetch_add(1, Ordering::Relaxed);
    }
  }

  /// Record that the node's task died unexpectedly — its run loop exited by
  /// panic or other abnormal termination, *not* a normal stop/teardown. Counted
  /// separately from `errored` (a per-message outcome) so a node death is
  /// observable as a distinct event. The runtime's per-node supervisor calls
  /// this once when it detects its actor task has exited abnormally.
  pub fn record_death(&self) {
    self.died.fetch_add(1, Ordering::Relaxed);
  }

  /// Record that one delivery was **quarantined** as poison: its cross-delivery
  /// attempt count crossed the node's `poison_after` threshold, so it was
  /// diverted without being handled. Counted separately from `errored` (a
  /// per-message handle failure) and `died` (a node-lifecycle event) so a poison
  /// drop is observable as its own distinct event. The run loop calls this only
  /// on the fallback path — a node with **no** dead-letter sink granted; with a
  /// sink the poison is recorded on the sink (reason `Poison`) instead.
  pub fn record_poison(&self) {
    self.poisoned.fetch_add(1, Ordering::Relaxed);
  }

  pub fn handled(&self) -> u64 {
    self.handled.load(Ordering::Relaxed)
  }

  pub fn errored(&self) -> u64 {
    self.errored.load(Ordering::Relaxed)
  }

  /// How many times this node's task has died unexpectedly (panic / abnormal
  /// exit). `0` for a healthy node; a normal stop/teardown does **not** bump it.
  pub fn died(&self) -> u64 {
    self.died.load(Ordering::Relaxed)
  }

  /// How many deliveries this node quarantined as poison and dropped on the
  /// no-sink fallback (the threshold was crossed but no dead-letter sink was
  /// granted to preserve them). `0` for a node that has quarantined nothing, or
  /// one whose poison was absorbed by a dead-letter sink.
  pub fn poisoned(&self) -> u64 {
    self.poisoned.load(Ordering::Relaxed)
  }
}

/// Where the outcome of handling a delivery goes. Every delivery carries one —
/// there is always somewhere the outcome is reported.
pub enum Ack {
  /// At-most-once (pre-write): fold the outcome into shared health counters.
  Health(Arc<Health>),
  /// At-least-once (queue-fed): send the outcome back to the feeder, which
  /// turns it into `complete`/`fail`. If this is dropped *without* reporting —
  /// the delivery was shed, or the actor died mid-handle — the receiver
  /// observes a closed channel, which the feeder treats as a failure and
  /// retries. The retry-on-loss path is automatic, not hand-coded.
  Complete(oneshot::Sender<Outcome>),
}

impl Ack {
  /// Report the handling outcome. Consumes the ack: it fires exactly once.
  pub fn report(self, outcome: Outcome) {
    match self {
      Ack::Health(health) => health.record(&outcome),
      Ack::Complete(tx) => {
        // Receiver may be gone (feeder gave up / timed out); that's fine.
        let _ = tx.send(outcome);
      }
    }
  }
}

/// A message plus the ack that reports how handling it went. This is what flows
/// through a mailbox into an actor.
///
/// It also carries the **trace context** at the point of construction (`span`):
/// each hop's `Delivery::new` captures the current span, so the receiving
/// actor's handle span can be parented by it. That's how a trace follows a
/// message across the mailbox/task boundary — the causal link that
/// `#[instrument]` alone can't cross.
///
/// Alongside the span it carries a [`CorrelationId`] — the **run** the message
/// belongs to — on the exact same rails: [`Delivery::new`] captures the current
/// correlation just as it captures the current span, so a run id flows
/// trigger → emit → hop without any actor forwarding it.
pub struct Delivery {
  pub msg: Message,
  pub ack: Ack,
  /// The span active where this delivery was produced — the parent for the
  /// receiver's handle span. Disabled (near-free) when no subscriber is active.
  pub span: Span,
  /// The run this message belongs to — captured from the current correlation
  /// (the task-local set by the runtime before each `handle`), so it propagates
  /// across the hop without an actor touching it.
  pub correlation: CorrelationId,
  /// How many times *this same delivery* has been handed to a node — a
  /// **cross-delivery** attempt count that the poison-quarantine gate reads. A
  /// first/normal delivery is `1`; an at-least-once feeder that re-delivers a
  /// `Lost` message stamps an incremented count via
  /// [`with_attempts`](Delivery::with_attempts), so a message that keeps
  /// crashing the node climbs past the node's `poison_after` and is quarantined
  /// instead of looping. It is a bare `u32` carried beside `correlation`/`span`,
  /// so the hot path stays allocation-free.
  ///
  /// Distinct from a `retry` policy's *in-handler* re-invocations (those happen
  /// within one delivery): `attempts` counts how many *separate* deliveries the
  /// feeder has made of the same message across deaths/re-feeds. The in-memory
  /// `Delivery` resets to `1` on a process restart — persisting it is the
  /// durable feeder's concern, not the transport's.
  pub attempts: u32,
}

impl Delivery {
  /// Construct a delivery, capturing the current trace span **and** the current
  /// correlation (the run in scope on this task). On an internal hop — an
  /// actor's emit, routed inside its scoped `handle` — this inherits the
  /// handling run's id automatically. With no run in scope (a cold,
  /// trigger-side construction with nothing to correlate to) a fresh id is
  /// minted, so a delivery always names a run.
  pub fn new(msg: Message, ack: Ack) -> Self {
    Self::with_correlation(msg, ack, CorrelationId::current().unwrap_or_default())
  }

  /// Construct a delivery with an **explicit** correlation — used at a trigger
  /// (`Engine::push`), which mints (`CorrelationId::new()`) or adopts an
  /// external/parent run id *before* the run starts, rather than inheriting one
  /// from a scope. Still captures the current trace span.
  pub fn with_correlation(msg: Message, ack: Ack, correlation: CorrelationId) -> Self {
    Self {
      msg,
      ack,
      span: Span::current(),
      correlation,
      // A fresh construction is a *first* attempt. A re-delivering feeder
      // overrides this with `with_attempts`.
      attempts: 1,
    }
  }

  /// Stamp this delivery's cross-delivery [`attempts`](Delivery::attempts)
  /// count, returning `self` for chaining off a constructor. Used by an
  /// at-least-once feeder (e.g. `Engine::push_durable`) to carry the feeder's
  /// current attempt number onto the delivery, so the runtime's poison gate can
  /// tell a fresh delivery (`1`) from a re-delivery (`> 1`) and quarantine a
  /// message that keeps crashing the node. `0` is normalized to `1` — a delivery
  /// always counts as at least one attempt.
  pub fn with_attempts(mut self, attempts: u32) -> Self {
    self.attempts = attempts.max(1);
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn err() -> Outcome {
    Err(ActorError::Handle("boom".to_owned()))
  }

  #[test]
  fn health_counts_handled_and_errored() {
    let h = Health::default();
    h.record(&Ok(()));
    h.record(&err());
    assert_eq!(h.handled(), 2);
    assert_eq!(h.errored(), 1);
  }

  #[test]
  fn record_death_is_separate_from_errored() {
    let h = Health::default();
    h.record(&err()); // a failed message
    h.record_death(); // a node death
    assert_eq!(h.errored(), 1);
    assert_eq!(h.died(), 1);
    // The death did not inflate the per-message counters.
    assert_eq!(h.handled(), 1);
  }

  #[test]
  fn record_poison_is_separate_from_errored_and_died() {
    let h = Health::default();
    h.record(&err()); // a failed message
    h.record_death(); // a node death
    h.record_poison(); // a quarantined poison message
    assert_eq!(h.errored(), 1);
    assert_eq!(h.died(), 1);
    assert_eq!(h.poisoned(), 1);
    // The poison did not inflate the per-message handled counter.
    assert_eq!(h.handled(), 1);
  }

  #[test]
  fn delivery_defaults_to_first_attempt() {
    let d = Delivery::new(
      Message::empty("x"),
      Ack::Health(Arc::new(Health::default())),
    );
    assert_eq!(d.attempts, 1);
  }

  #[test]
  fn with_attempts_stamps_and_normalizes_zero_to_one() {
    let d = Delivery::new(
      Message::empty("x"),
      Ack::Health(Arc::new(Health::default())),
    )
    .with_attempts(4);
    assert_eq!(d.attempts, 4);
    // A feeder that hands `0` still counts as one attempt.
    let d = Delivery::new(
      Message::empty("x"),
      Ack::Health(Arc::new(Health::default())),
    )
    .with_attempts(0);
    assert_eq!(d.attempts, 1);
  }

  #[test]
  fn health_ack_folds_outcome_into_counters() {
    let health = Arc::new(Health::default());
    Ack::Health(health.clone()).report(err());
    assert_eq!(health.handled(), 1);
    assert_eq!(health.errored(), 1);
  }

  #[tokio::test]
  async fn complete_ack_sends_outcome_to_feeder() {
    let (tx, rx) = oneshot::channel();
    Ack::Complete(tx).report(Ok(()));
    assert!(rx.await.expect("feeder receives outcome").is_ok());
  }

  #[tokio::test]
  async fn dropped_complete_ack_closes_receiver() {
    let (tx, rx) = oneshot::channel::<Outcome>();
    let ack = Ack::Complete(tx);
    drop(ack); // delivery shed, or actor died before reporting
    assert!(rx.await.is_err()); // feeder sees closed → retry
  }
}
