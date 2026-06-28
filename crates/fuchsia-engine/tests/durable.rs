//! `push_durable`: the at-least-once, awaited-outcome ingress. These mirror the
//! transport's Complete-ack tests, one rung up — through the engine's router and
//! a real running actor rather than a bare oneshot.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
};
use fuchsia_engine::{Engine, EngineError};

// ---- Ok actor (handles successfully, counts what it handled) ----

struct OkActor {
  handled: Arc<AtomicUsize>,
}

impl Actor for OkActor {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    self.handled.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }
  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct OkCreator {
  handled: Arc<AtomicUsize>,
}

impl ActorCreator for OkCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(OkActor {
      handled: self.handled.clone(),
    }))
  }
}

// ---- Err actor (handler returns an error) ----

struct ErrActor;

impl Actor for ErrActor {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    Err(ActorError::Handle("boom".to_owned()))
  }
  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct ErrCreator;

impl ActorCreator for ErrCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(ErrActor))
  }
}

// ---- Panic actor (dies mid-handle, dropping its ack unreported) ----

struct PanicActor;

impl Actor for PanicActor {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    panic!("actor dies mid-handle");
  }
  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct PanicCreator;

impl ActorCreator for PanicCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(PanicActor))
  }
}

// ---- Gated actor (blocks in handle until the test opens a gate) ----

/// Blocks in `handle` until the gate is opened, then passes every message
/// through. Lets a test hold the entrypoint's mailbox full and prove
/// `push_durable` *waits* (backpressure) rather than shedding.
struct GatedActor {
  gate: Arc<(Mutex<bool>, Condvar)>,
}

impl Actor for GatedActor {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    let (lock, cvar) = &*self.gate;
    let mut open = lock.lock().unwrap();
    while !*open {
      open = cvar.wait(open).unwrap();
    }
    Ok(())
  }
  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct GatedCreator {
  gate: Arc<(Mutex<bool>, Condvar)>,
}

impl ActorCreator for GatedCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(GatedActor {
      gate: self.gate.clone(),
    }))
  }
}

// ---- Tests ----

#[tokio::test]
async fn push_durable_resolves_ok_when_handled() {
  let handled = Arc::new(AtomicUsize::new(0));

  let engine = Engine::new();
  engine
    .register(
      "ok",
      OkCreator {
        handled: handled.clone(),
      },
    )
    .await;
  engine
    .add_node(
      ActorId::new("entry"),
      "ok",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Resolving Ok *is* the at-least-once confirmation that the node handled it.
  engine
    .push_durable(&ActorId::new("entry"), Message::empty("job"))
    .await
    .unwrap();

  assert_eq!(handled.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn push_durable_surfaces_handler_error() {
  let engine = Engine::new();
  engine.register("err", ErrCreator).await;
  engine
    .add_node(
      ActorId::new("entry"),
      "err",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Ok(Err(..)) from the runner → the retriable handler-error variant, carrying
  // the original `ActorError` so the caller can decide to dead-letter a poison.
  let err = engine
    .push_durable(&ActorId::new("entry"), Message::empty("job"))
    .await
    .unwrap_err();
  assert!(matches!(err, EngineError::Handle(ActorError::Handle(_))));
}

#[tokio::test]
async fn push_durable_to_unknown_node_is_not_found() {
  let engine = Engine::new();

  let err = engine
    .push_durable(&ActorId::new("missing"), Message::empty("job"))
    .await
    .unwrap_err();
  assert!(matches!(err, EngineError::NotFound(_)));
}

// `EngineError::Undelivered` (the router still holds the entrypoint's sender but
// its mailbox receiver is already gone) has no test here: it is the zombie state
// that arises only once an actor's task has died while its routing entry still
// lingers, which the public API can't reproduce deterministically until death
// detection lands (`node-failure-handling`). Add the test alongside that work.

#[tokio::test]
async fn push_durable_reports_lost_when_actor_dies_mid_handle() {
  let engine = Engine::new();
  engine.register("panic", PanicCreator).await;
  engine
    .add_node(
      ActorId::new("entry"),
      "panic",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // The entrypoint panics while handling: tokio aborts its task, dropping the
  // Complete ack unreported, so the oneshot closes → `Lost` (retry-on-loss).
  // (A panic backtrace is printed; the test still passes.)
  let err = engine
    .push_durable(&ActorId::new("entry"), Message::empty("job"))
    .await
    .unwrap_err();
  assert!(matches!(err, EngineError::Lost));
}

/// Must exceed `fuchsia-runtime`'s mailbox capacity (32) so the buffer fills and
/// the overflow exercises `push_durable`'s blocking send rather than a shed.
const OVERFLOW: usize = 40;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn push_durable_waits_on_a_full_mailbox_instead_of_shedding() {
  let gate = Arc::new((Mutex::new(false), Condvar::new()));

  let engine = Arc::new(Engine::new());
  engine
    .register("gated", GatedCreator { gate: gate.clone() })
    .await;
  engine
    .add_node(
      ActorId::new("entry"),
      "gated",
      &ActorConfig::default(),
      ActorCapabilities::new(),
    )
    .await
    .unwrap();

  // Fire more invocations than the mailbox can hold. The first is stuck in
  // `handle` (gate closed); the rest fill the buffer and then block on `send`.
  let mut tasks = Vec::with_capacity(OVERFLOW);
  for _ in 0..OVERFLOW {
    let engine = engine.clone();
    tasks.push(tokio::spawn(async move {
      engine
        .push_durable(&ActorId::new("entry"), Message::empty("job"))
        .await
    }));
  }

  // Give a (hypothetical) shedding path time to resolve early: had `push_durable`
  // used the shedding `offer`, the overflow deliveries would have dropped their
  // acks and these tasks would have resolved to `Lost` by now. With backpressure,
  // nothing completes while the gate is closed.
  tokio::time::sleep(Duration::from_millis(50)).await;
  assert!(
    tasks.iter().all(|t| !t.is_finished()),
    "push_durable shed instead of waiting on a full mailbox"
  );

  // Open the gate; every queued invocation drains and is handled.
  {
    let (lock, cvar) = &*gate;
    *lock.lock().unwrap() = true;
    cvar.notify_all();
  }

  for t in tasks {
    t.await.unwrap().unwrap();
  }
}
