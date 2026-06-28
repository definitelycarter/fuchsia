use std::sync::Arc;

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorFactory, ActorId, Message,
};
use fuchsia_transport::{Ack, Delivery, Health, MailboxRx, MailboxTx, mailbox};

use crate::error::RuntimeError;
use crate::registry::{ActorHandle, ActorRegistry};
use crate::schedule::TokioSchedule;

pub struct Runtime {
  factory: ActorFactory,
  registry: ActorRegistry,
}

impl Runtime {
  pub fn new() -> Self {
    Self {
      factory: ActorFactory::new(),
      registry: ActorRegistry::new(),
    }
  }

  pub fn register(&mut self, type_name: impl Into<String>, creator: impl ActorCreator) {
    self.factory.register(type_name, creator);
  }

  pub async fn spawn(
    &mut self,
    actor_id: ActorId,
    type_name: &str,
    config: &ActorConfig,
  ) -> Result<(), RuntimeError> {
    self
      .spawn_with_caps(actor_id, type_name, config, ActorCapabilities::new())
      .await
      .map(|_| ())
  }

  fn context(actor_id: &ActorId) -> ActorContext {
    ActorContext::new(
      actor_id.to_string(),
      actor_id.to_string(),
      actor_id.to_string(),
    )
  }

  /// Spawn an actor with a caller-populated capability bag, returning its
  /// mailbox + health so the caller (the engine) can route to it. The caller
  /// puts `emit` and any host/binding capabilities in `caps`;
  /// `schedule` is layered in here, since it needs the mailbox this method
  /// creates. A plain [`Runtime::spawn`] passes an empty bag.
  pub async fn spawn_with_caps(
    &mut self,
    actor_id: ActorId,
    type_name: &str,
    config: &ActorConfig,
    caps: ActorCapabilities,
  ) -> Result<(MailboxTx, Arc<Health>), RuntimeError> {
    if self.registry.contains(&actor_id) {
      return Err(RuntimeError::AlreadyRunning(actor_id));
    }

    // The mailbox and health exist before construction so the scheduler can
    // hold a weak handle back to this actor's own mailbox (timers deliver
    // there). The actor is then built with the full capability bundle.
    let (tx, rx) = mailbox(32);
    let health = Arc::new(Health::default());
    let caps = caps.with_schedule(Arc::new(TokioSchedule {
      mailbox: tx.downgrade(),
      health: health.clone(),
    }));

    let mut actor = self.factory.create(type_name, config, &caps)?;
    let ctx = Self::context(&actor_id);

    // Setup is awaited before the task is spawned. On failure the actor is
    // dropped (its Drop impl releases any partial state) and nothing is
    // registered.
    actor.setup(&ctx).await.map_err(RuntimeError::Actor)?;

    tokio::spawn(run_actor(actor, ctx, rx));

    self.registry.insert(ActorHandle::new(
      actor_id,
      type_name.to_owned(),
      tx.clone(),
      health.clone(),
    ));

    Ok((tx, health))
  }

  pub async fn deliver(&self, actor_id: &ActorId, msg: Message) -> Result<(), RuntimeError> {
    let handle = self
      .registry
      .get(actor_id)
      .ok_or_else(|| RuntimeError::ActorNotFound(actor_id.clone()))?;

    let delivery = Delivery::new(msg, Ack::Health(handle.health().clone()));
    handle
      .mailbox()
      .send(delivery)
      .await
      .map_err(|_| RuntimeError::Send("mailbox closed".to_owned()))
  }

  pub fn stop(&mut self, actor_id: &ActorId) -> Result<(), RuntimeError> {
    self
      .registry
      .remove(actor_id)
      .ok_or_else(|| RuntimeError::ActorNotFound(actor_id.clone()))?;
    // dropping the handle closes tx, which closes rx in the task,
    // causing the actor loop to exit and teardown to run
    Ok(())
  }
}

impl Default for Runtime {
  fn default() -> Self {
    Self::new()
  }
}

async fn run_actor(mut actor: Box<dyn Actor>, ctx: ActorContext, mut rx: MailboxRx) {
  use tracing::Instrument;
  while let Some(delivery) = rx.recv().await {
    let Delivery {
      msg,
      ack,
      span: parent,
    } = delivery;
    // The handle span is a child of the upstream's span (carried on the
    // delivery), so a trace follows the message across this mailbox hop. The
    // actor's own emits, made inside this span, propagate it onward. DEBUG so
    // it's off the hot path unless tracing is turned up.
    let span =
      tracing::debug_span!(parent: &parent, "actor.handle", node = %ctx.node_id, kind = %msg.type_);
    // `.instrument(span).await` enters the span for the duration of the async
    // handle without holding a `!Send` span guard across the await point.
    let outcome = actor.handle(&ctx, msg).instrument(span).await;
    ack.report(outcome);
  }

  let _ = actor.teardown(&ctx).await;
}

#[cfg(test)]
mod tests {
  use super::*;
  use fuchsia_actor::{ActorError, ActorId, MessageValue, Schedule, async_trait};
  use std::sync::Arc;
  use std::sync::Mutex;
  use std::sync::atomic::{AtomicBool, Ordering};
  use tokio::sync::Notify;

  // ---- Echo actor (used by the basic tests) ----

  struct EchoActor;

  #[async_trait]
  impl Actor for EchoActor {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
    async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
      Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
  }

  struct EchoCreator;

  impl ActorCreator for EchoCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(EchoActor))
    }
  }

  // ---- Probe actor (observes lifecycle events) ----

  struct Probe {
    setup_called: AtomicBool,
    teardown_called: AtomicBool,
    received: Mutex<Vec<Message>>,
    notify: Notify,
  }

  impl Probe {
    fn new() -> Arc<Self> {
      Arc::new(Self {
        setup_called: AtomicBool::new(false),
        teardown_called: AtomicBool::new(false),
        received: Mutex::new(Vec::new()),
        notify: Notify::new(),
      })
    }
  }

  struct ProbeActor {
    probe: Arc<Probe>,
  }

  #[async_trait]
  impl Actor for ProbeActor {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      self.probe.setup_called.store(true, Ordering::SeqCst);
      Ok(())
    }
    async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
      self.probe.received.lock().unwrap().push(msg);
      self.probe.notify.notify_one();
      Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      self.probe.teardown_called.store(true, Ordering::SeqCst);
      self.probe.notify.notify_one();
      Ok(())
    }
  }

  struct ProbeCreator {
    probe: Arc<Probe>,
  }

  impl ActorCreator for ProbeCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(ProbeActor {
        probe: self.probe.clone(),
      }))
    }
  }

  // ---- Failing-setup actor (for the setup-failure scenario) ----

  struct FailingSetupActor;

  #[async_trait]
  impl Actor for FailingSetupActor {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Err(ActorError::Setup("intentional".to_owned()))
    }
    async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
      Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
  }

  struct FailingSetupCreator;

  impl ActorCreator for FailingSetupCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      _caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(FailingSetupActor))
    }
  }

  // ---- Helpers ----

  fn runtime() -> Runtime {
    let mut rt = Runtime::new();
    rt.register("echo", EchoCreator);
    rt
  }

  fn actor_id(s: &str) -> ActorId {
    ActorId::new(s)
  }

  // ---- Basic tests ----

  #[tokio::test]
  async fn spawn_registers_actor() {
    let mut rt = runtime();
    rt.spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .unwrap();
    assert!(rt.registry.contains(&actor_id("a")));
  }

  #[tokio::test]
  async fn spawn_duplicate_returns_error() {
    let mut rt = runtime();
    rt.spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .unwrap();
    let err = rt
      .spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::AlreadyRunning(_)));
  }

  #[tokio::test]
  async fn spawn_unknown_type_returns_error() {
    let mut rt = runtime();
    let err = rt
      .spawn(actor_id("a"), "unknown", &ActorConfig::default())
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::Actor(_)));
  }

  #[tokio::test]
  async fn deliver_to_running_actor_succeeds() {
    let mut rt = runtime();
    rt.spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .unwrap();
    let result = rt.deliver(&actor_id("a"), Message::empty("test")).await;
    assert!(result.is_ok());
  }

  #[tokio::test]
  async fn deliver_to_missing_actor_returns_error() {
    let rt = runtime();
    let err = rt
      .deliver(&actor_id("missing"), Message::empty("test"))
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::ActorNotFound(_)));
  }

  #[tokio::test]
  async fn stop_unregisters_actor() {
    let mut rt = runtime();
    rt.spawn(actor_id("a"), "echo", &ActorConfig::default())
      .await
      .unwrap();
    rt.stop(&actor_id("a")).unwrap();
    assert!(!rt.registry.contains(&actor_id("a")));
  }

  #[tokio::test]
  async fn stop_missing_actor_returns_error() {
    let mut rt = runtime();
    let err = rt.stop(&actor_id("missing")).err().unwrap();
    assert!(matches!(err, RuntimeError::ActorNotFound(_)));
  }

  // ---- Lifecycle tests ----

  #[tokio::test]
  async fn spawn_calls_setup() {
    let probe = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    );
    rt.spawn(actor_id("a"), "probe", &ActorConfig::default())
      .await
      .unwrap();
    // setup runs synchronously inside spawn, so this is observable immediately
    assert!(probe.setup_called.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn handle_receives_message() {
    let probe = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    );
    rt.spawn(actor_id("a"), "probe", &ActorConfig::default())
      .await
      .unwrap();

    let msg = Message::json("test", serde_json::json!({"value": 42}));
    rt.deliver(&actor_id("a"), msg).await.unwrap();

    probe.notify.notified().await;

    let received = probe.received.lock().unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].type_, "test");
    assert!(matches!(received[0].value, MessageValue::Json(_)));
  }

  #[tokio::test]
  async fn stop_triggers_teardown() {
    let probe = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe",
      ProbeCreator {
        probe: probe.clone(),
      },
    );
    rt.spawn(actor_id("a"), "probe", &ActorConfig::default())
      .await
      .unwrap();
    rt.stop(&actor_id("a")).unwrap();

    probe.notify.notified().await;

    assert!(probe.teardown_called.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn dropping_runtime_triggers_teardown() {
    let probe = Probe::new();
    {
      let mut rt = Runtime::new();
      rt.register(
        "probe",
        ProbeCreator {
          probe: probe.clone(),
        },
      );
      rt.spawn(actor_id("a"), "probe", &ActorConfig::default())
        .await
        .unwrap();
    }
    // rt is dropped here; the handle's tx is dropped; the actor task
    // sees rx close and runs teardown

    probe.notify.notified().await;

    assert!(probe.teardown_called.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn multiple_actors_run_independently() {
    let probe_a = Probe::new();
    let probe_b = Probe::new();
    let mut rt = Runtime::new();
    rt.register(
      "probe_a",
      ProbeCreator {
        probe: probe_a.clone(),
      },
    );
    rt.register(
      "probe_b",
      ProbeCreator {
        probe: probe_b.clone(),
      },
    );

    rt.spawn(actor_id("a"), "probe_a", &ActorConfig::default())
      .await
      .unwrap();
    rt.spawn(actor_id("b"), "probe_b", &ActorConfig::default())
      .await
      .unwrap();

    rt.deliver(&actor_id("a"), Message::empty("for-a"))
      .await
      .unwrap();
    probe_a.notify.notified().await;

    assert_eq!(probe_a.received.lock().unwrap().len(), 1);
    assert_eq!(probe_a.received.lock().unwrap()[0].type_, "for-a");
    assert!(probe_b.received.lock().unwrap().is_empty());
  }

  #[tokio::test]
  async fn setup_failure_does_not_register() {
    let mut rt = Runtime::new();
    rt.register("failing", FailingSetupCreator);

    let err = rt
      .spawn(actor_id("a"), "failing", &ActorConfig::default())
      .await
      .err()
      .unwrap();
    assert!(matches!(err, RuntimeError::Actor(ActorError::Setup(_))));
    assert!(!rt.registry.contains(&actor_id("a")));
  }

  // ---- Scheduler actor (schedules a delayed message to itself) ----

  struct SchedulerProbe {
    fired: Arc<AtomicBool>,
    notify: Arc<Notify>,
    schedule: Arc<dyn Schedule>,
  }

  #[async_trait]
  impl Actor for SchedulerProbe {
    async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
    async fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
      match msg.type_.as_str() {
        "go" => self
          .schedule
          .schedule_self(std::time::Duration::from_millis(5), Message::empty("tick")),
        "tick" => {
          self.fired.store(true, Ordering::SeqCst);
          self.notify.notify_one();
        }
        _ => {}
      }
      Ok(())
    }
    async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
      Ok(())
    }
  }

  struct SchedulerCreator {
    fired: Arc<AtomicBool>,
    notify: Arc<Notify>,
  }

  impl ActorCreator for SchedulerCreator {
    fn create(
      &self,
      _config: &ActorConfig,
      caps: &ActorCapabilities,
    ) -> Result<Box<dyn Actor>, ActorError> {
      Ok(Box::new(SchedulerProbe {
        fired: self.fired.clone(),
        notify: self.notify.clone(),
        schedule: caps.schedule(),
      }))
    }
  }

  #[tokio::test]
  async fn schedule_self_delivers_a_timer_message() {
    let fired = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let mut rt = Runtime::new();
    rt.register(
      "scheduler",
      SchedulerCreator {
        fired: fired.clone(),
        notify: notify.clone(),
      },
    );
    rt.spawn(actor_id("a"), "scheduler", &ActorConfig::default())
      .await
      .unwrap();

    // "go" makes the actor schedule a "tick" to itself; the timer delivers it
    // back into its own mailbox, where it's handled like any message.
    rt.deliver(&actor_id("a"), Message::empty("go"))
      .await
      .unwrap();
    notify.notified().await;

    assert!(fired.load(Ordering::SeqCst));
  }
}
