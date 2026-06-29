//! Proves the per-message correlation id is minted at the trigger and
//! propagated automatically by the runtime/engine — actors never manage it:
//!
//! 1. One id flows unchanged trigger → A → `emit` → B across a mailbox hop: the
//!    id `push` mints lands on the downstream node's `ctx.execution_id`, even
//!    though the relaying node never touches it.
//! 2. Two interleaved runs through a *shared* node keep distinct ids: each
//!    `handle` sees its own run's id, not a static per-actor label.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Emit,
  Message, async_trait,
};
use fuchsia_actor_builtins::PassthroughCreator;
use fuchsia_engine::{CorrelationId, Engine};
use tokio::sync::Notify;

/// Records the `execution_id` (the run's correlation) seen on each `handle`, and
/// re-emits the message so it can also sit *mid-graph* and forward downstream.
struct Relay {
  seen: Arc<Mutex<Vec<String>>>,
  emit: Arc<dyn Emit>,
  notify: Arc<Notify>,
}

#[async_trait]
impl Actor for Relay {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.seen.lock().unwrap().push(ctx.execution_id.clone());
    self.emit.emit(msg);
    self.notify.notify_one();
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct RelayCreator {
  seen: Arc<Mutex<Vec<String>>>,
  notify: Arc<Notify>,
}

impl ActorCreator for RelayCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Relay {
      seen: self.seen.clone(),
      emit: caps.emit(),
      notify: self.notify.clone(),
    }))
  }
}

#[tokio::test]
async fn id_flows_unchanged_from_trigger_through_emit_to_downstream() {
  let a_seen = Arc::new(Mutex::new(Vec::new()));
  let b_seen = Arc::new(Mutex::new(Vec::new()));
  let b_notify = Arc::new(Notify::new());

  let engine = Engine::new();
  // A is a passthrough (never touches correlation); B records what it sees.
  engine.register("passthrough", PassthroughCreator).await;
  engine
    .register(
      "relay_a",
      RelayCreator {
        seen: a_seen.clone(),
        notify: Arc::new(Notify::new()),
      },
    )
    .await;
  engine
    .register(
      "relay_b",
      RelayCreator {
        seen: b_seen.clone(),
        notify: b_notify.clone(),
      },
    )
    .await;

  engine
    .add_node(
      ActorId::new("a"),
      "relay_a",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("b"),
      "relay_b",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_default_edge(ActorId::new("a"), ActorId::new("b"))
    .unwrap();

  // Mint the run id at the trigger and push it in. A relays → routes to B.
  engine
    .push(
      &ActorId::new("a"),
      Message::empty("ping"),
      CorrelationId::from("run-A"),
    )
    .unwrap();

  tokio::time::timeout(Duration::from_secs(1), b_notify.notified())
    .await
    .expect("B handled the relayed message");

  // The entry node A and the downstream node B both saw the *same* trigger id,
  // unchanged — neither ever forwarded it by hand.
  assert_eq!(*a_seen.lock().unwrap(), vec!["run-A".to_owned()]);
  assert_eq!(*b_seen.lock().unwrap(), vec!["run-A".to_owned()]);
}

#[tokio::test]
async fn interleaved_runs_through_a_shared_node_keep_distinct_ids() {
  let en_seen = Arc::new(Mutex::new(Vec::new()));
  let s_seen = Arc::new(Mutex::new(Vec::new()));
  let s_notify = Arc::new(Notify::new());

  let engine = Engine::new();
  engine.register("passthrough", PassthroughCreator).await;
  engine
    .register(
      "enrich",
      RelayCreator {
        seen: en_seen.clone(),
        notify: Arc::new(Notify::new()),
      },
    )
    .await;
  engine
    .register(
      "respond",
      RelayCreator {
        seen: s_seen.clone(),
        notify: s_notify.clone(),
      },
    )
    .await;

  // Two runs share both nodes: trigger → enrich → respond.
  engine
    .add_node(
      ActorId::new("enrich"),
      "enrich",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_node(
      ActorId::new("respond"),
      "respond",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();
  engine
    .add_default_edge(ActorId::new("enrich"), ActorId::new("respond"))
    .unwrap();

  // Push two runs through the same entrypoint with distinct ids.
  engine
    .push(
      &ActorId::new("enrich"),
      Message::empty("r1-msg"),
      CorrelationId::from("r1"),
    )
    .unwrap();
  engine
    .push(
      &ActorId::new("enrich"),
      Message::empty("r2-msg"),
      CorrelationId::from("r2"),
    )
    .unwrap();

  // Wait until the shared terminal node has handled both runs.
  while s_seen.lock().unwrap().len() < 2 {
    tokio::time::timeout(Duration::from_secs(1), s_notify.notified())
      .await
      .expect("respond handled a message");
  }

  // Each run kept its own id on each handle through the shared node — not a
  // single static node-id label shared by both.
  let mut seen = s_seen.lock().unwrap().clone();
  seen.sort();
  assert_eq!(seen, vec!["r1".to_owned(), "r2".to_owned()]);

  // And the upstream shared node saw both too.
  let mut en = en_seen.lock().unwrap().clone();
  en.sort();
  assert_eq!(en, vec!["r1".to_owned(), "r2".to_owned()]);
}
