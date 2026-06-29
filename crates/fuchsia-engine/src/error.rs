use fuchsia_actor::{ActorError, ActorId};
use fuchsia_runtime::RuntimeError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
  #[error("runtime: {0}")]
  Runtime(#[from] RuntimeError),
  #[error("router lock poisoned")]
  Lock,
  #[error("node not found: {0}")]
  NotFound(ActorId),
  /// `add_edge` only: the source node declares a [`Fixed`] set of output ports
  /// and `port` is not one of them (nor the always-allowed `"out"` / reserved
  /// `"error"`). A typo'd or non-existent port, rejected up front rather than
  /// silently never routing. `Dynamic` nodes never produce this.
  ///
  /// [`Fixed`]: fuchsia_actor::OutputPorts::Fixed
  #[error("node {node} has no output port {port:?}")]
  UnknownPort { node: ActorId, port: String },
  /// `add_edge` only: the edge would close a cycle — either a self-loop
  /// (`from == to`) or an edge whose target `to` already reaches its source
  /// `from` over the existing edges. Fuchsia graphs are acyclic, so the edge is
  /// rejected and the graph left unchanged. See the
  /// [DAG enforcement](../rfcs/dag-enforcement.md) RFC.
  #[error("edge {from} -> {to} would create a cycle")]
  Cycle { from: ActorId, to: ActorId },
  /// `push_durable` only: the entrypoint handled the message but its handler
  /// returned an error — retriable, though a persistent failure is a poison
  /// candidate the caller may dead-letter.
  #[error("entrypoint handler errored: {0}")]
  Handle(ActorError),
  /// `push_durable` only: the entrypoint's mailbox was gone before the message
  /// could be enqueued (the node was torn down). The message was *never*
  /// handled, so a retry cannot duplicate — transient, retry freely.
  #[error("entrypoint mailbox gone; message undelivered")]
  Undelivered,
  /// `push_durable` only: the message was enqueued but no handle outcome came
  /// back — the delivery was shed, or the actor died mid-handle, closing the ack
  /// channel. It *may* already have been handled, so a retry can duplicate —
  /// transient, retry but dedupe.
  #[error("entrypoint handle outcome lost")]
  Lost,
}
