use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
  async_trait,
};
use fuchsia_runtime::Runtime;
use tokio::sync::Notify;

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

struct NotifyingActor {
  notify: Arc<Notify>,
}

#[async_trait]
impl Actor for NotifyingActor {
  async fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  async fn handle(&mut self, _ctx: &ActorContext, _msg: Message) -> Result<(), ActorError> {
    self.notify.notify_one();
    Ok(())
  }
  async fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct NotifyingCreator {
  notify: Arc<Notify>,
}

impl ActorCreator for NotifyingCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(NotifyingActor {
      notify: self.notify.clone(),
    }))
  }
}

fn tokio_runtime() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap()
}

fn bench_spawn(c: &mut Criterion) {
  let tokio_rt = tokio_runtime();
  let counter = AtomicU64::new(0);

  c.bench_function("runtime/spawn", |b| {
    b.iter_batched(
      || {
        let mut runtime = Runtime::new();
        runtime.register("echo", EchoCreator);
        let id = ActorId::new(format!("a-{}", counter.fetch_add(1, Ordering::Relaxed)));
        (runtime, id)
      },
      |(mut runtime, id)| {
        let result = tokio_rt.block_on(runtime.spawn(id, "echo", &ActorConfig::default()));
        (runtime, result)
      },
      BatchSize::SmallInput,
    )
  });
}

fn bench_deliver(c: &mut Criterion) {
  let tokio_rt = tokio_runtime();
  let mut runtime = Runtime::new();
  runtime.register("echo", EchoCreator);
  let id = ActorId::new("a");
  tokio_rt
    .block_on(runtime.spawn(id.clone(), "echo", &ActorConfig::default()))
    .unwrap();

  c.bench_function("runtime/deliver", |b| {
    b.iter(|| tokio_rt.block_on(runtime.deliver(&id, Message::empty("bench"))))
  });
}

fn bench_roundtrip(c: &mut Criterion) {
  let tokio_rt = tokio_runtime();
  let notify = Arc::new(Notify::new());
  let mut runtime = Runtime::new();
  runtime.register(
    "notifying",
    NotifyingCreator {
      notify: notify.clone(),
    },
  );
  let id = ActorId::new("a");
  tokio_rt
    .block_on(runtime.spawn(id.clone(), "notifying", &ActorConfig::default()))
    .unwrap();

  c.bench_function("runtime/roundtrip", |b| {
    b.iter(|| {
      tokio_rt.block_on(async {
        runtime.deliver(&id, Message::empty("bench")).await.unwrap();
        notify.notified().await;
      })
    })
  });
}

criterion_group!(benches, bench_spawn, bench_deliver, bench_roundtrip);
criterion_main!(benches);
