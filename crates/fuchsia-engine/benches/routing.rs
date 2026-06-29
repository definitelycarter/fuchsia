//! Routing hot-path microbench: drive `RoutedEmit::emit` → `RouterState::route`
//! (plus the per-port counter bump) directly, *without* the source's actor task
//! or recv loop, so the measurement is the routing + counters cost itself.
//!
//! `engine.emit_sink(source)` hands back the same `emit` sink a node is given at
//! `add_node`; calling `.emit_to("out", msg)` runs exactly the path an actor's
//! emission takes. Sinks are real `add_node`'d actors (draining mailboxes), so
//! offers land as `Delivered` and the counter records a delivery per edge.
//!
//! Two shapes: route to **one** successor, and **fan out** to W successors on a
//! single port (W = 1, 4, 16). Per-input throughput; divide by W for per-edge.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Emit,
  Message, async_trait,
};
use fuchsia_engine::Engine;

/// A sink that drops every message — keeps the bench on routing, not on the
/// sink's work. Its mailbox still drains (its task receives and discards), so
/// offers from the routing path land as `Delivered`.
struct DrainActor;

#[async_trait]
impl Actor for DrainActor {
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

struct DrainCreator;

impl ActorCreator for DrainCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(DrainActor))
  }
}

fn tokio_runtime() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_multi_thread()
    .worker_threads(2)
    .enable_all()
    .build()
    .expect("build tokio runtime")
}

/// Build an engine with a registered `source` node and `width` drain sinks, all
/// wired from the source's `"out"` port. Returns the engine and the source's
/// emit sink (the routing entry point under test).
fn wire(tokio_rt: &tokio::runtime::Runtime, width: usize) -> (Engine, Arc<dyn Emit>) {
  tokio_rt.block_on(async {
    let engine = Engine::new();
    engine.register("drain", DrainCreator).await;

    let source = ActorId::new("source");
    // The source itself is a drain node too — only so its `NodeCounters` exist
    // and the routing path records a per-port counter (the realistic case).
    engine
      .add_node(
        source.clone(),
        "drain",
        &ActorConfig::default(),
        ActorCapabilities::new(),
      )
      .await
      .expect("add source");

    for i in 0..width {
      let sink = ActorId::new(format!("sink-{i}"));
      engine
        .add_node(
          sink.clone(),
          "drain",
          &ActorConfig::default(),
          ActorCapabilities::new(),
        )
        .await
        .expect("add sink");
      engine
        .add_default_edge(source.clone(), sink)
        .expect("wire edge");
    }

    let sink = engine.emit_sink(source);
    (engine, sink)
  })
}

/// Build an engine with `n` independent source nodes, each wired to its own
/// drain sink, and return each source's emit sink. Models the real shape: many
/// actors emitting concurrently — where a global per-emit counter lock would
/// serialize them and a per-node counter does not.
fn wire_sources(tokio_rt: &tokio::runtime::Runtime, n: usize) -> (Engine, Vec<Arc<dyn Emit>>) {
  tokio_rt.block_on(async {
    let engine = Engine::new();
    engine.register("drain", DrainCreator).await;

    let mut sinks = Vec::with_capacity(n);
    for i in 0..n {
      let source = ActorId::new(format!("source-{i}"));
      let target = ActorId::new(format!("sink-{i}"));
      for id in [&source, &target] {
        engine
          .add_node(
            id.clone(),
            "drain",
            &ActorConfig::default(),
            ActorCapabilities::new(),
          )
          .await
          .expect("add node");
      }
      engine
        .add_default_edge(source.clone(), target)
        .expect("wire edge");
      sinks.push(engine.emit_sink(source));
    }
    (engine, sinks)
  })
}

/// Emit through every sink concurrently from one thread per sink, for the given
/// number of rounds. Returns once all threads finish. This is where removing
/// the global hot-path mutex shows up: the threads route in parallel.
fn drive_concurrent(sinks: &[Arc<dyn Emit>], rounds: usize) {
  let start = Arc::new(AtomicBool::new(false));
  thread::scope(|scope| {
    let mut handles = Vec::with_capacity(sinks.len());
    for sink in sinks {
      let sink = Arc::clone(sink);
      let start = Arc::clone(&start);
      handles.push(scope.spawn(move || {
        while !start.load(Ordering::Acquire) {
          std::hint::spin_loop();
        }
        for _ in 0..rounds {
          sink.emit_to("out", Message::empty("bench"));
        }
      }));
    }
    // Release all threads together so they actually contend.
    start.store(true, Ordering::Release);
    for h in handles {
      let _ = h.join();
    }
  });
}

fn bench_route_concurrent(c: &mut Criterion) {
  let tokio_rt = tokio_runtime();
  let mut group = c.benchmark_group("engine/route_concurrent");

  // Rounds per thread per iteration — enough work that thread spawn/join isn't
  // the dominant cost, so the routing path (and any lock on it) shows through.
  const ROUNDS: usize = 2_000;

  let mut keepalive = Vec::new();
  for threads in [2_usize, 4, 8] {
    let (engine, sinks) = wire_sources(&tokio_rt, threads);
    group.throughput(Throughput::Elements((threads * ROUNDS) as u64));
    group.bench_with_input(BenchmarkId::from_parameter(threads), &threads, |b, _| {
      b.iter(|| drive_concurrent(&sinks, ROUNDS));
    });
    keepalive.push((engine, sinks));
  }
  group.finish();
  drop(keepalive);
}

fn bench_route_single(c: &mut Criterion) {
  let tokio_rt = tokio_runtime();
  // Keep the engine alive for the whole bench so sinks keep draining.
  let (_engine, sink) = wire(&tokio_rt, 1);

  c.bench_function("engine/route_single", |b| {
    b.iter(|| {
      sink.emit_to("out", Message::empty("bench"));
    })
  });
}

fn bench_route_fanout(c: &mut Criterion) {
  let tokio_rt = tokio_runtime();
  let mut group = c.benchmark_group("engine/route_fanout");

  // Keep every engine alive for the whole group so the sink actors keep
  // draining while their width is benched (and after).
  let mut keepalive = Vec::new();
  for width in [1_usize, 4, 16] {
    let (engine, sink) = wire(&tokio_rt, width);
    group.throughput(Throughput::Elements(1));
    group.bench_with_input(BenchmarkId::from_parameter(width), &width, |b, _| {
      b.iter(|| {
        sink.emit_to("out", Message::empty("bench"));
      })
    });
    keepalive.push((engine, sink));
  }
  group.finish();
  drop(keepalive);
}

criterion_group!(
  benches,
  bench_route_single,
  bench_route_fanout,
  bench_route_concurrent
);
criterion_main!(benches);
