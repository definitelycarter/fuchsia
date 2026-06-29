//! Turn a handler closure into an [`Actor`] — no `struct`, no `impl`, no empty
//! lifecycle methods — for the trivial glue, test-fixture, and example nodes
//! where the `struct` + [`ActorCreator`](crate::ActorCreator) + `impl Actor`
//! ceremony outweighs the behaviour. The produced actor routes and emits exactly
//! like a hand-written one; only the *handler code* is spelled differently.
//!
//! Two forms, both yielding a `Box<dyn Actor>` with the trait's default no-op
//! `setup`/`teardown`:
//!
//! - [`from_fn`] — a stateless handler.
//! - [`from_fn_with_state`] — state owned by the adapter, handed to the handler
//!   as `&mut S` each call.
//!
//! ## The two open mechanics, resolved
//!
//! **`emit` is held by the adapter and passed in.** An actor's `emit` capability
//! is injected at *construction*, not available to a free-standing closure, so
//! the adapter is built with the `Arc<dyn Emit>` and hands a refcount-bumped
//! clone to the handler on every call. The closure stays free of capture
//! ceremony — it just names an `emit` argument.
//!
//! **`ActorContext` is passed by value.** The handler returns a `'static`
//! future, so it cannot borrow `&ActorContext` across the future's `.await`
//! points — the classic pre-async-closure lifetime friction. Passing the context
//! by value (cloning its three small ids) sidesteps that borrow and keeps the
//! bounds simple; it ties into the roadmap's `Arc<str>` ids, which would turn the
//! clone into a refcount bump.
//!
//! Because the future is `'static`, any **state mutation happens synchronously**
//! in the closure body, before the future is produced — the returned future does
//! not hold the `&mut S` borrow:
//!
//! ```
//! use std::sync::Arc;
//! use fuchsia_actor::{from_fn_with_state, ActorCapabilities};
//!
//! let caps = ActorCapabilities::new();
//! let node = from_fn_with_state(0u64, caps.emit(), |count, _ctx, msg, emit| {
//!   *count += 1; // synchronous: not borrowed across the await
//!   let seq = *count;
//!   async move {
//!     emit.emit_to("out", msg);
//!     let _ = seq;
//!     Ok(())
//!   }
//! });
//! # let _ = node;
//! ```

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;

use crate::actor::{Actor, ActorContext, Emit, Message};
use crate::error::ActorError;

/// Wrap a stateless handler closure as an [`Actor`].
///
/// `emit` is the sink the produced actor was built with (typically
/// `caps.emit()` inside a creator); a refcount-bumped clone is handed to the
/// handler on every message. Lifecycle defaults to no-op.
pub fn from_fn<F, Fut>(emit: Arc<dyn Emit>, handler: F) -> Box<dyn Actor>
where
  F: FnMut(ActorContext, Message, Arc<dyn Emit>) -> Fut + Send + 'static,
  Fut: Future<Output = Result<(), ActorError>> + Send + 'static,
{
  Box::new(FromFn { emit, handler })
}

/// Wrap a stateful handler closure as an [`Actor`]. `init` is the actor's
/// starting state, owned by the adapter and handed to the handler as `&mut S`
/// each call. As with [`from_fn`], `emit` is held by the adapter and passed in,
/// and lifecycle defaults to no-op.
pub fn from_fn_with_state<S, F, Fut>(init: S, emit: Arc<dyn Emit>, handler: F) -> Box<dyn Actor>
where
  S: Send + 'static,
  F: FnMut(&mut S, ActorContext, Message, Arc<dyn Emit>) -> Fut + Send + 'static,
  Fut: Future<Output = Result<(), ActorError>> + Send + 'static,
{
  Box::new(FromFnWithState {
    state: init,
    emit,
    handler,
  })
}

/// The stateless adapter: the held sink plus the handler.
struct FromFn<F> {
  emit: Arc<dyn Emit>,
  handler: F,
}

#[async_trait]
impl<F, Fut> Actor for FromFn<F>
where
  F: FnMut(ActorContext, Message, Arc<dyn Emit>) -> Fut + Send + 'static,
  Fut: Future<Output = Result<(), ActorError>> + Send + 'static,
{
  async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    // Refcount bump so the handler owns its sink for the call.
    let emit = Arc::clone(&self.emit);
    // Context by value (see the module docs): the `'static` future can't borrow
    // `ctx`, and the three ids are cheap to clone.
    (self.handler)(ctx.clone(), msg, emit).await
  }
}

/// The stateful adapter: owned state, the held sink, and the handler.
struct FromFnWithState<S, F> {
  state: S,
  emit: Arc<dyn Emit>,
  handler: F,
}

#[async_trait]
impl<S, F, Fut> Actor for FromFnWithState<S, F>
where
  S: Send + 'static,
  F: FnMut(&mut S, ActorContext, Message, Arc<dyn Emit>) -> Fut + Send + 'static,
  Fut: Future<Output = Result<(), ActorError>> + Send + 'static,
{
  async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    let emit = Arc::clone(&self.emit);
    // `state` and `handler` are disjoint fields, so both can be borrowed at once;
    // the `'static` future does not retain the `&mut state` borrow past the call.
    (self.handler)(&mut self.state, ctx.clone(), msg, emit).await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::actor::ActorCapabilities;
  use std::sync::Mutex;

  /// A shared log of the `(port, message-type)` of every emission.
  type Log = Arc<Mutex<Vec<(String, String)>>>;

  /// Records the `(port, message-type)` of every emission.
  struct Recorder(Log);

  impl Emit for Recorder {
    fn emit_to(&self, port: &str, msg: Message) {
      self.0.lock().unwrap().push((port.to_owned(), msg.type_));
    }
  }

  fn recorder() -> (Arc<dyn Emit>, Log) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let emit: Arc<dyn Emit> = Arc::new(Recorder(log.clone()));
    (emit, log)
  }

  fn ctx() -> ActorContext {
    ActorContext::new("exec", "node", 1)
  }

  #[tokio::test]
  async fn stateless_from_fn_emits_through_the_held_sink() {
    let (emit, log) = recorder();
    let mut actor = from_fn(emit, |_ctx, msg, emit| async move {
      emit.emit(msg);
      Ok(())
    });

    actor
      .handle(&ctx(), Message::empty("reading"))
      .await
      .unwrap();

    assert_eq!(
      *log.lock().unwrap(),
      vec![("out".to_owned(), "reading".to_owned())]
    );
  }

  #[tokio::test]
  async fn stateful_from_fn_mutates_across_messages() {
    let (emit, log) = recorder();
    // Counts inputs and emits the running count on the default port.
    let mut actor = from_fn_with_state(0u64, emit, |count, _ctx, _msg, emit| {
      *count += 1;
      let seq = *count;
      async move {
        emit.emit(Message::json("count", serde_json::json!(seq)));
        Ok(())
      }
    });

    for _ in 0..3 {
      actor.handle(&ctx(), Message::empty("tick")).await.unwrap();
    }

    let log = log.lock().unwrap();
    assert_eq!(log.len(), 3);
    assert!(
      log
        .iter()
        .all(|(port, type_)| port == "out" && type_ == "count")
    );
  }

  #[tokio::test]
  async fn from_fn_can_select_a_named_port() {
    let (emit, log) = recorder();
    let mut actor = from_fn(emit, |_ctx, msg, emit| async move {
      emit.emit_to("left", msg);
      Ok(())
    });

    actor.handle(&ctx(), Message::empty("x")).await.unwrap();

    assert_eq!(log.lock().unwrap()[0].0, "left");
  }

  #[tokio::test]
  async fn default_lifecycle_is_a_noop() {
    // A `from_fn` actor implements only `handle`; the trait's default
    // `setup`/`teardown` must succeed untouched.
    let mut actor = from_fn(
      ActorCapabilities::new().emit(),
      |_ctx, _msg, _emit| async move { Ok(()) },
    );
    actor.setup(&ctx()).await.unwrap();
    actor.teardown(&ctx()).await.unwrap();
  }

  #[tokio::test]
  async fn handler_error_propagates() {
    let mut actor = from_fn(
      ActorCapabilities::new().emit(),
      |_ctx, _msg, _emit| async move { Err(ActorError::Handle("boom".to_owned())) },
    );
    let err = actor.handle(&ctx(), Message::empty("x")).await.unwrap_err();
    assert!(matches!(err, ActorError::Handle(m) if m == "boom"));
  }
}
