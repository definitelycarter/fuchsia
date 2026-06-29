//! Proves a trace follows a message across the mailbox/task boundary: the
//! handle span of a downstream actor is parented by the upstream actor's handle
//! span, which is parented by the root span at the push site. That parent chain
//! is what `#[instrument]` alone can't produce (each actor runs on its own
//! task); it works because `Delivery` carries the producing span.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  async_trait,
};
use fuchsia_actor_builtins::PassthroughCreator;
use fuchsia_engine::Engine;
use tokio::sync::Notify;
use tracing::Subscriber;
use tracing::span::{Attributes, Id};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// Span name + optional parent id, keyed by span id.
type SpanMap = Arc<Mutex<HashMap<u64, (String, Option<u64>)>>>;

/// Records each span's name and parent id, so a test can assert ancestry.
#[derive(Clone, Default)]
struct Spans(SpanMap);

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Spans {
  fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
    // The explicit `parent:` if set (handle spans), else the contextual parent
    // (the entered root), else none.
    let parent = attrs
      .parent()
      .cloned()
      .or_else(|| ctx.current_span().id().cloned())
      .map(|p| p.into_u64());
    self
      .0
      .lock()
      .unwrap()
      .insert(id.into_u64(), (attrs.metadata().name().to_owned(), parent));
  }
}

/// Terminal actor that signals when it has handled a message.
struct Sink(Arc<Notify>);
#[async_trait]
impl Actor for Sink {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    self.0.notify_one();
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct SinkCreator(Arc<Notify>);
impl ActorCreator for SinkCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Sink(self.0.clone())))
  }
}

#[tokio::test]
async fn trace_follows_a_message_across_the_mailbox_hop() {
  let spans = Spans::default();
  // Current-thread runtime (tokio::test default) + thread-local subscriber, so
  // the spawned actor tasks share this subscriber.
  let _guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(spans.clone()));

  let notify = Arc::new(Notify::new());
  let engine = Engine::new();
  engine.register("passthrough", PassthroughCreator).await;
  engine.register("sink", SinkCreator(notify.clone())).await;

  engine
    .add_node(
      ActorId::new("a"),
      "passthrough",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("b"),
      "sink",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_default_edge(ActorId::new("a"), ActorId::new("b"))
    .unwrap();

  // Push within a root span; passthrough (a) re-emits → routes to sink (b).
  tracing::info_span!("ingress").in_scope(|| {
    engine
      .push(&ActorId::new("a"), Message::empty("ping"))
      .unwrap();
  });

  tokio::time::timeout(Duration::from_secs(1), notify.notified())
    .await
    .expect("sink handled the message");

  // Assert the chain: ingress → a.handle → b.handle.
  let spans = spans.0.lock().unwrap();
  let ingress = spans
    .iter()
    .find(|(_, (name, parent))| name == "ingress" && parent.is_none())
    .map(|(id, _)| *id)
    .expect("ingress root span");

  let handles: Vec<(u64, Option<u64>)> = spans
    .iter()
    .filter(|(_, (name, _))| name == "actor.handle")
    .map(|(id, (_, parent))| (*id, *parent))
    .collect();
  assert_eq!(handles.len(), 2, "one handle span per actor");

  let (a_handle, _) = handles
    .iter()
    .find(|(_, parent)| *parent == Some(ingress))
    .expect("upstream handle is parented by the ingress root");
  assert!(
    handles.iter().any(|(_, parent)| *parent == Some(*a_handle)),
    "downstream handle is parented by the upstream handle — trace crossed the hop"
  );
}
