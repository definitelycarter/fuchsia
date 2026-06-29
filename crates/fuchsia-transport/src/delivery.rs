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
#[derive(Debug, Default)]
pub struct Health {
  handled: AtomicU64,
  errored: AtomicU64,
  died: AtomicU64,
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
    }
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
