//! Per-emit fan-out cost: one passthrough entry pushes each message to W sinks.
//!
//! Each iteration: spawn the workflow, push N messages (each fan-outs W ways),
//! close entry, await teardown. Throughput is reported per input message;
//! divide by W to get per-edge cost if you want it.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use fuchsia_runtime::{ActorRegistry, Orchestrator};
use std::sync::Arc;
use tokio::runtime::Runtime;

mod common;
use common::{bench_msg, fan_out, registry};

const MESSAGES_PER_ITER: u64 = 1_000;

fn bench_fan_out(c: &mut Criterion) {
  let rt = Runtime::new().expect("build tokio runtime");
  let reg: Arc<ActorRegistry> = Arc::new(registry());

  let mut group = c.benchmark_group("fan_out");
  group.throughput(Throughput::Elements(MESSAGES_PER_ITER));

  for &w in &[2usize, 8, 32] {
    let graph = fan_out(w);
    group.bench_with_input(BenchmarkId::new("width", w), &w, |b, _| {
      b.to_async(&rt).iter(|| {
        let reg = reg.clone();
        let graph = graph.clone();
        async move {
          let orch = Orchestrator::new(reg);
          let handle = orch.start(&graph).expect("start workflow");
          for i in 0..MESSAGES_PER_ITER {
            handle.send(bench_msg(i)).await.expect("send into entry");
          }
          let results = handle.join().await;
          black_box(results);
        }
      });
    });
  }

  group.finish();
}

criterion_group!(benches, bench_fan_out);
criterion_main!(benches);
