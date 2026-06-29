use fuchsia_actor::{ActorId, Message};

use crate::correlation::CorrelationId;

/// A host-provided sink for messages a node can no longer process — exhausted
/// `retry` or a `fail`-policy stop — so they are **preserved** (inspectable /
/// replayable) rather than dropped + counted.
///
/// This is the *seam*: fuchsia owns the trait (so the runtime can call it), the
/// **product owns the store** — where a dead letter actually lands (a DB table,
/// a Kafka/SQS DLQ, an n8n error-execution record, an HA log + notification, or
/// nowhere) is the product's choice, exactly because upstream applications
/// differ. It is the same shape as every other domain capability: not one of
/// fuchsia's two universal capabilities (`emit` / `schedule`), but inserted by
/// the product under its own trait type into the
/// [`ActorCapabilities`](fuchsia_actor::ActorCapabilities) bag —
/// `caps.insert::<dyn DeadLetter>(arc)` — and pulled by the runtime via
/// `caps.get::<dyn DeadLetter>()`. A node without one granted falls back to
/// today's count-and-drop on [`Health`](crate::Health); dead-lettering is
/// optional.
///
/// It lives here in `fuchsia-transport` (not `fuchsia-actor`) because a dead
/// letter is keyed by its [`CorrelationId`] — the run id, owned by this crate,
/// which the bottom `fuchsia-actor` crate cannot reference. This is the same
/// failure-plumbing crate that already owns `Delivery` / `Ack` / `Health`, so
/// the dead-letter sink sits naturally beside them.
///
/// `dead_letter` is synchronous and infallible (returns `()`): it runs on the
/// actor's run loop between `handle` calls, so blocking it would stall the node;
/// a product that needs durable, fallible storage offloads it (hand the
/// [`DeadLettered`] to a channel / spawn its own writer) and absorbs its own
/// errors — keeping the runtime's failure path non-blocking and non-throwing,
/// like [`Emit::emit_to`](fuchsia_actor::Emit::emit_to).
pub trait DeadLetter: Send + Sync {
  /// Take responsibility for one message the runtime can no longer process.
  /// Called **once** per dead-lettered delivery.
  fn dead_letter(&self, letter: DeadLettered);
}

/// One dead-lettered message and everything needed to triage or replay it: the
/// original [`Message`], the run it belonged to ([`CorrelationId`]), the node
/// that failed it ([`ActorId`]), and *why* it ended up here ([`DeadLetterReason`]).
///
/// `#[non_exhaustive]` so future preservation context (an attempt count for a
/// poison message, a timestamp) can be added without breaking a product's
/// construction or destructuring — products read it through field access, not by
/// building it.
#[derive(Debug)]
#[non_exhaustive]
pub struct DeadLettered {
  /// The original message that could not be processed — moved in whole, so the
  /// product can persist or replay it.
  pub msg: Message,
  /// The run this message belonged to, so the dead letter ties back to the
  /// originating request — dead letters are *keyed by correlation id*.
  pub correlation: CorrelationId,
  /// The node that failed the message.
  pub node: ActorId,
  /// Why the message was dead-lettered.
  pub reason: DeadLetterReason,
}

impl DeadLettered {
  /// Build a dead letter. A constructor (rather than a public struct literal)
  /// because [`DeadLettered`] is `#[non_exhaustive]` — that lets future context
  /// fields be added without breaking a *cross-crate* caller (the runtime, which
  /// builds these), while products only ever *read* the fields. The fields are
  /// taken in the order they triage: the failed message, its run, its node, why.
  pub fn new(
    msg: Message,
    correlation: CorrelationId,
    node: ActorId,
    reason: DeadLetterReason,
  ) -> Self {
    Self {
      msg,
      correlation,
      node,
      reason,
    }
  }
}

/// Why a message was dead-lettered.
///
/// `#[non_exhaustive]` so a future `Poison { attempts }` reason (the
/// per-delivery attempt threshold, a later slice) slots in without breaking a
/// product's exhaustive `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeadLetterReason {
  /// A `retry` policy exhausted its budget — every attempt errored.
  RetryExhausted {
    /// Total `handle` attempts made before giving up (`1 + max` retries).
    attempts: u32,
    /// The error string from the final failed attempt.
    error: String,
  },
  /// A `fail` policy stopped the node on an errored `handle`; the triggering
  /// message is preserved here before the node dies.
  Failed {
    /// The error string from the failed `handle`.
    error: String,
  },
  /// The node **died permanently** — it crashed (a panic, or an abnormal exit)
  /// and exhausted its restart budget — so the runtime drains whatever was still
  /// queued in its mailbox here rather than dropping it. Unlike [`Failed`] (one
  /// triggering message on a deliberate `fail`) this is each *bystander* message
  /// that was waiting behind a crash. `restarts` is how many times the node was
  /// rebuilt before giving up.
  ///
  /// [`Failed`]: DeadLetterReason::Failed
  NodeDied {
    /// How many times the node was restarted before its budget was exhausted and
    /// it died permanently (`0` for a node that crashed with no budget to spend,
    /// e.g. a default `max_restarts: 0` node would never reach this drain).
    restarts: u32,
  },
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  /// A recording sink — the shape a product's real impl takes, minus the store.
  struct Recorder(Mutex<Vec<DeadLettered>>);

  impl DeadLetter for Recorder {
    fn dead_letter(&self, letter: DeadLettered) {
      self.0.lock().expect("lock").push(letter);
    }
  }

  #[test]
  fn sink_receives_the_dead_letter() {
    let rec = Recorder(Mutex::new(Vec::new()));
    rec.dead_letter(DeadLettered::new(
      Message::empty("boom"),
      CorrelationId::from("run-1"),
      ActorId::new("n"),
      DeadLetterReason::Failed {
        error: "handle failed: x".to_owned(),
      },
    ));
    let got = rec.0.lock().expect("lock");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].msg.type_, "boom");
    assert_eq!(got[0].correlation.as_str(), "run-1");
    assert_eq!(got[0].node, ActorId::new("n"));
  }

  #[test]
  fn node_died_reason_carries_restart_count() {
    // The permanent-death drain reason: a bystander message preserved after the
    // node crashed and exhausted its restart budget.
    let rec = Recorder(Mutex::new(Vec::new()));
    rec.dead_letter(DeadLettered::new(
      Message::empty("bystander"),
      CorrelationId::from("run-2"),
      ActorId::new("n"),
      DeadLetterReason::NodeDied { restarts: 3 },
    ));
    let got = rec.0.lock().expect("lock");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].reason, DeadLetterReason::NodeDied { restarts: 3 });
  }
}
