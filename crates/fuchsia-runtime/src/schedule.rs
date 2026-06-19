use std::sync::Arc;
use std::time::Duration;

use fuchsia_actor::{Message, Schedule};
use fuchsia_transport::{Ack, Delivery, Health, WeakMailboxTx};

/// In-process timer scheduler: delivers a delayed message back into an actor's
/// own mailbox, so timers flow through the normal `handle` path.
///
/// Holds a **weak** mailbox handle — a pending timer never keeps a torn-down
/// actor alive. On fire it upgrades; if the actor is gone, the message drops.
/// The delivery carries the actor's own `Health` ack (at-most-once, like any
/// self-wake).
pub(crate) struct TokioSchedule {
  pub(crate) mailbox: WeakMailboxTx,
  pub(crate) health: Arc<Health>,
}

impl Schedule for TokioSchedule {
  fn schedule_self(&self, after: Duration, msg: Message) {
    let mailbox = self.mailbox.clone();
    let health = self.health.clone();
    tokio::spawn(async move {
      tokio::time::sleep(after).await;
      if let Some(tx) = mailbox.upgrade() {
        let _ = tx.offer(Delivery::new(msg, Ack::Health(health)));
      }
    });
  }
}
