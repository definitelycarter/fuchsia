//! Regression: `Engine::add_node` runs an actor's async `setup` **outside** the
//! runtime lock, so a slow setup on one node does not serialize provisioning of
//! others. Two nodes whose async `setup` each takes `DELAY`, provisioned
//! concurrently, finish in ~one `DELAY` — not two. If `setup` ran while holding
//! the runtime lock, the second `add_node` would wait for the first and the
//! bound below would fail.

use std::sync::Arc;
use std::time::{Duration, Instant};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  async_trait,
};
use fuchsia_engine::Engine;

/// An actor whose `setup` awaits — a stand-in for setup I/O (opening a
/// connection, subscribing to a topic).
struct SlowSetup {
  delay: Duration,
}

#[async_trait]
impl Actor for SlowSetup {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    tokio::time::sleep(self.delay).await;
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct SlowSetupCreator {
  delay: Duration,
}

impl ActorCreator for SlowSetupCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(SlowSetup { delay: self.delay }))
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_add_node_setups_overlap() {
  let delay = Duration::from_millis(100);
  let engine = Arc::new(Engine::new());
  engine
    .register("slow-setup", SlowSetupCreator { delay })
    .await;

  let add = |id: &str| {
    let engine = engine.clone();
    let id = ActorId::new(id);
    async move {
      engine
        .add_node(
          id,
          "slow-setup",
          &ActorConfig::default(),
          ActorCapabilities::new(),
        )
        .await
    }
  };

  let start = Instant::now();
  let (a, b) = tokio::join!(add("a"), add("b"));
  let elapsed = start.elapsed();
  a.unwrap();
  b.unwrap();

  // Serial (setup under the lock) would be ~2·delay (200ms); overlapping setups
  // (setup outside the lock) are ~delay (100ms). The bound discriminates the two.
  assert!(
    elapsed < delay * 3 / 2,
    "concurrent add_node setups serialized ({elapsed:?}); setup is holding the runtime lock"
  );
}
