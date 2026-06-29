use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Identifies the **run** a message belongs to — minted once at the trigger
/// that starts a run, then carried unchanged through every `emit`/route hop and
/// across the guest boundary. It answers "which run / which originating event
/// does this message belong to?", so an error, a metric, or a final result can
/// be tied back to the request that caused it.
///
/// It rides per-*delivery* metadata, exactly like the trace [`Span`](tracing::Span):
/// the runtime sets it as the current correlation before each `handle` (see
/// [`CorrelationId::scope`]) and [`Delivery::new`](crate::Delivery::new) captures
/// it. Actors and guests therefore **never manage it** — input-to-output
/// propagation is automatic.
///
/// Opaque and cheap to clone: the id is an `Arc<str>`, so a clone is a refcount
/// bump (it rides on every delivery, like the span). [`Display`](fmt::Display)
/// renders it for traces and for [`ActorContext::execution_id`].
///
/// [`ActorContext::execution_id`]: fuchsia_actor::ActorContext::execution_id
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CorrelationId(Arc<str>);

/// Process-global source of fresh ids. Monotonic, so a minted id is unique
/// within the process — enough for in-process run correlation (durability is a
/// later concern).
static NEXT: AtomicU64 = AtomicU64::new(1);

impl CorrelationId {
  /// Mint a fresh, process-unique id — what a trigger calls when it has nothing
  /// external to correlate to (`Engine::push(.., CorrelationId::new())`).
  pub fn new() -> Self {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    Self(Arc::from(format!("cid-{n}")))
  }

  /// The id as a string slice.
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// The inner `Arc<str>`, cheaply shared. This is a refcount bump (not an
  /// allocation), so the runtime can hand the correlation straight into a
  /// per-message [`ActorContext::execution_id`] without copying the string.
  ///
  /// [`ActorContext::execution_id`]: fuchsia_actor::ActorContext::execution_id
  pub fn as_arc(&self) -> Arc<str> {
    // Refcount bump of the shared id — the whole point of the `Arc<str>` newtype.
    Arc::clone(&self.0)
  }

  /// Run `fut` with `self` as the current correlation — a task-local, the
  /// analog of entering a tracing span for the duration of a future. Any
  /// [`Delivery::new`](crate::Delivery::new) constructed inside `fut` (an
  /// actor's emit, a scheduled self-message) captures it, so the run id
  /// propagates input → output without the actor touching it.
  pub fn scope<F: Future>(self, fut: F) -> impl Future<Output = F::Output> {
    CURRENT.scope(self, fut)
  }

  /// The correlation in scope on the current task, if one is set.
  // Clippy would prefer the `Result::ok` shorthand, but the repo bans that in
  // production (it reads as silently discarding an error). The only error here
  // is `AccessError` — "no correlation in scope" — which `None` faithfully
  // means, so the explicit match is the intent, not a discarded error.
  #[allow(clippy::manual_ok_err)]
  pub fn current() -> Option<CorrelationId> {
    match CURRENT.try_with(|c| c.clone()) {
      Ok(id) => Some(id),
      Err(_) => None,
    }
  }
}

impl Default for CorrelationId {
  fn default() -> Self {
    Self::new()
  }
}

impl fmt::Display for CorrelationId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// Adopt an existing id — an external request/trace id, or a parent run's id —
/// so a run can be correlated to something that already has an identity.
impl From<String> for CorrelationId {
  fn from(s: String) -> Self {
    Self(Arc::from(s))
  }
}

impl From<&str> for CorrelationId {
  fn from(s: &str) -> Self {
    Self(Arc::from(s))
  }
}

impl From<Arc<str>> for CorrelationId {
  fn from(s: Arc<str>) -> Self {
    Self(s)
  }
}

tokio::task_local! {
  /// The correlation in scope on the current task, set by the runtime for the
  /// duration of each `handle` (mirroring how a tracing span is entered).
  static CURRENT: CorrelationId;
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn new_ids_are_distinct() {
    assert_ne!(CorrelationId::new(), CorrelationId::new());
  }

  #[test]
  fn adopts_an_external_id_and_displays_it() {
    let id = CorrelationId::from("req-42");
    assert_eq!(id.as_str(), "req-42");
    assert_eq!(id.to_string(), "req-42");
  }

  #[test]
  fn clone_is_equal() {
    let id = CorrelationId::from("run-1");
    assert_eq!(id.clone(), id);
  }

  #[test]
  fn as_arc_shares_the_inner_id() {
    let id = CorrelationId::from("run-9");
    let arc = id.as_arc();
    assert_eq!(&*arc, "run-9");
    // Same allocation, shared — handing it out is a refcount bump.
    assert!(Arc::ptr_eq(&arc, &id.as_arc()));
  }

  #[test]
  fn current_is_none_outside_a_scope() {
    assert!(CorrelationId::current().is_none());
  }

  #[tokio::test]
  async fn scope_sets_the_current_correlation() {
    let id = CorrelationId::from("run-7");
    let seen = id.clone().scope(async { CorrelationId::current() }).await;
    assert_eq!(seen, Some(id));
    // Leaves no current correlation once the scope ends.
    assert!(CorrelationId::current().is_none());
  }
}
