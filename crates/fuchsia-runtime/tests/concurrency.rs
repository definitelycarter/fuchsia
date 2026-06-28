//! The payoff of the async `Actor` contract: an awaited I/O call inside `handle`
//! yields the runtime thread instead of blocking it, so actors run concurrently.
//!
//! On a **single-threaded** runtime, N actors that each `.await` a `DELAY` in
//! `handle` finish in roughly one `DELAY` — not N of them. If `handle` blocked
//! the thread, the single worker would serialize them into ~N·DELAY, and the
//! bound below would fail.

use std::time::{Duration, Instant};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  async_trait,
};
use fuchsia_runtime::Runtime;
use tokio::sync::mpsc;

/// An actor whose `handle` awaits a slow async operation — a stand-in for an I/O
/// capability like `fetch` — then reports completion down an mpsc channel.
struct SlowIo {
  delay: Duration,
  done: mpsc::UnboundedSender<()>,
}

#[async_trait]
impl Actor for SlowIo {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    // Awaiting yields the worker thread to other actors instead of blocking it.
    tokio::time::sleep(self.delay).await;
    let _ = self.done.send(());
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct SlowIoCreator {
  delay: Duration,
  done: mpsc::UnboundedSender<()>,
}

impl ActorCreator for SlowIoCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(SlowIo {
      delay: self.delay,
      done: self.done.clone(),
    }))
  }
}

#[tokio::test] // single-threaded runtime by default — exactly the point
async fn awaited_io_runs_concurrently_not_serially() {
  const N: usize = 10;
  let delay = Duration::from_millis(50);
  let (tx, mut rx) = mpsc::unbounded_channel();

  let mut rt = Runtime::new();
  rt.register("slow", SlowIoCreator { delay, done: tx });
  for i in 0..N {
    rt.spawn(ActorId::new(format!("a{i}")), "slow", &ActorConfig::default())
      .await
      .unwrap();
  }

  let start = Instant::now();
  for i in 0..N {
    rt.deliver(&ActorId::new(format!("a{i}")), Message::empty("go"))
      .await
      .unwrap();
  }
  for _ in 0..N {
    rx.recv().await.expect("a completion");
  }
  let elapsed = start.elapsed();

  // Serial/blocking would be ~N·delay (500ms); concurrent async is ~delay
  // (50ms). A generous bound that still fails loudly if handles block the
  // single worker thread.
  assert!(
    elapsed < delay * (N as u32) / 2,
    "expected concurrent async handles (~{delay:?}); took {elapsed:?} — handles look serial/blocking"
  );
}
