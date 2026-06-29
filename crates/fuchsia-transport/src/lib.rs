//! Message delivery plumbing for fuchsia: the [`Delivery`] (a message plus the
//! [`Ack`] that reports how handling it went) and the bounded [`mailbox`] that
//! carries deliveries into an actor.
//!
//! There is deliberately no `Transport` trait. Actors always read from a
//! channel mailbox. Durability is layered *in front*: a queue feeder pushes
//! deliveries carrying an [`Ack::Complete`] (at-least-once), while pre-write
//! producers push deliveries carrying an [`Ack::Health`] (at-most-once). The
//! runner is uniform — it calls [`Ack::report`] after `handle`, and what that
//! does (complete/fail a job, or fold into health counters) is the ack's
//! concern, not the runner's.

mod correlation;
mod delivery;
mod mailbox;

pub use correlation::CorrelationId;
pub use delivery::{Ack, Delivery, Health, Outcome};
pub use mailbox::{MailboxRx, MailboxTx, Offer, WeakMailboxTx, mailbox};
