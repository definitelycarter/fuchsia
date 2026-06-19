use tokio::sync::mpsc::{self, error::TrySendError};

use crate::delivery::Delivery;

/// Result of a non-blocking [`MailboxTx::offer`].
#[derive(Debug, PartialEq, Eq)]
pub enum Offer {
  /// Buffered into the mailbox.
  Delivered,
  /// Mailbox full — the delivery was dropped (at-most-once shedding).
  Shed,
  /// No receiver left.
  Closed,
}

/// A bounded mailbox carrying [`Delivery`]s to one actor. The receiver is the
/// single consumer (the runner); senders may be cloned for fan-in.
pub fn mailbox(capacity: usize) -> (MailboxTx, MailboxRx) {
  let (tx, rx) = mpsc::channel(capacity);
  (MailboxTx(tx), MailboxRx(rx))
}

#[derive(Debug, Clone)]
pub struct MailboxTx(mpsc::Sender<Delivery>);

impl MailboxTx {
  /// Non-blocking send: drop the delivery if the mailbox is full. Used by
  /// at-most-once producers (pre-write), which prefer shedding a stale reading
  /// to blocking the source. A shed delivery drops its ack — so a shed
  /// `Complete` delivery signals the feeder to retry, for free.
  pub fn offer(&self, delivery: Delivery) -> Offer {
    match self.0.try_send(delivery) {
      Ok(()) => Offer::Delivered,
      Err(TrySendError::Full(_)) => Offer::Shed,
      Err(TrySendError::Closed(_)) => Offer::Closed,
    }
  }

  /// Blocking send: wait for room. Used by producers that must not drop and
  /// want backpressure — e.g. the queue feeder, which then stops claiming
  /// while the mailbox is full. Returns the delivery back if the receiver is
  /// gone.
  pub async fn send(&self, delivery: Delivery) -> Result<(), Delivery> {
    self.0.send(delivery).await.map_err(|e| e.0)
  }

  /// A non-owning handle to this mailbox. It does *not* keep the actor alive,
  /// so a pending timer holding one can't block teardown — the scheduler uses
  /// it to deliver a delayed message back to an actor, upgrading on fire.
  pub fn downgrade(&self) -> WeakMailboxTx {
    WeakMailboxTx(self.0.downgrade())
  }
}

/// A weak [`MailboxTx`] — held by timers so they don't keep a torn-down actor
/// alive. `upgrade` yields a usable sender only while the actor still lives.
#[derive(Clone)]
pub struct WeakMailboxTx(mpsc::WeakSender<Delivery>);

impl WeakMailboxTx {
  pub fn upgrade(&self) -> Option<MailboxTx> {
    self.0.upgrade().map(MailboxTx)
  }
}

pub struct MailboxRx(mpsc::Receiver<Delivery>);

impl MailboxRx {
  /// Pull the next delivery, suspending until one arrives. `None` once every
  /// sender has been dropped.
  pub async fn recv(&mut self) -> Option<Delivery> {
    self.0.recv().await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::delivery::{Ack, Delivery, Health, Outcome};
  use fuchsia_actor::Message;
  use std::sync::Arc;
  use tokio::sync::oneshot;

  fn health_delivery(type_: &str) -> Delivery {
    Delivery::new(
      Message::empty(type_),
      Ack::Health(Arc::new(Health::default())),
    )
  }

  #[tokio::test]
  async fn offer_then_recv_round_trips() {
    let (tx, mut rx) = mailbox(4);
    assert_eq!(tx.offer(health_delivery("a")), Offer::Delivered);
    let d = rx.recv().await.expect("delivery");
    assert_eq!(d.msg.type_, "a");
  }

  #[tokio::test]
  async fn offer_sheds_when_full() {
    let (tx, _rx) = mailbox(1);
    assert_eq!(tx.offer(health_delivery("a")), Offer::Delivered);
    assert_eq!(tx.offer(health_delivery("b")), Offer::Shed);
  }

  #[tokio::test]
  async fn shedding_a_complete_delivery_signals_retry() {
    let (tx, _rx) = mailbox(1);
    // Fill the single slot so the next offer is shed.
    assert_eq!(tx.offer(health_delivery("fill")), Offer::Delivered);

    let (ack_tx, ack_rx) = oneshot::channel::<Outcome>();
    let shed = Delivery::new(Message::empty("job"), Ack::Complete(ack_tx));
    assert_eq!(tx.offer(shed), Offer::Shed); // dropped here → ack_tx dropped

    assert!(ack_rx.await.is_err()); // feeder observes closed → retry
  }

  #[tokio::test]
  async fn recv_returns_none_when_all_senders_dropped() {
    let (tx, mut rx) = mailbox(1);
    drop(tx);
    assert!(rx.recv().await.is_none());
  }
}
