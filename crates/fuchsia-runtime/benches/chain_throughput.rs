//! End-to-end throughput through a linear chain of passthrough actors.
//!
//! Each iteration: spawn the workflow, push N messages, close entry, await all
//! actor tasks. Measures the full per-message cost amortized over a chain of
//! length K, including spawn and teardown.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use fuchsia_runtime::{ActorRegistry, Orchestrator};
use std::sync::Arc;
use tokio::runtime::Runtime;

mod common;
use common::{bench_msg, chain, registry};

const MESSAGES_PER_ITER: u64 = 1_000;

fn bench_chain(c: &mut Criterion) {
  let rt = Runtime::new().expect("build tokio runtime");
  let reg: Arc<ActorRegistry> = Arc::new(registry());

  let mut group = c.benchmark_group("chain_throughput");
  group.throughput(Throughput::Elements(MESSAGES_PER_ITER));

  for &k in &[1usize, 4, 16] {
    let graph = chain(k);
    group.bench_with_input(BenchmarkId::new("chain_len", k), &k, |b, _| {
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

criterion_group!(benches, bench_chain);
criterion_main!(benches);
