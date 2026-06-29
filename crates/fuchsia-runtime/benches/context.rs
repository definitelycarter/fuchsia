//! Isolates the per-delivery `ActorContext` construction — the one thing the
//! runtime rebuilds for *every message of every actor* in its recv→handle loop
//! (see `run_actor`'s `msg_ctx`). The `runtime/roundtrip` bench also exercises
//! this, but buries it under a tokio task hop and a `Notify`; this harness times
//! only the context build so the allocation profile is visible.
//!
//! It faithfully reproduces the runtime's per-message construction *call*: the
//! `execution_id` comes from the delivery's correlation (`CorrelationId::as_arc`
//! — a refcount bump), `node_id` is an `Arc::clone` of the actor's stable
//! spawn-time id (a refcount bump), and `task_id` is minted fresh (the one
//! genuine per-message allocation). This is the `context/per_message` headline
//! for the `Arc<str>` change.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use fuchsia_actor::ActorContext;
use fuchsia_transport::CorrelationId;

/// A fresh per-message task id, replicating `runtime::next_task_id` (one genuine
/// allocation per message — the single id with no stable source to share).
fn next_task_id() -> Arc<str> {
  static NEXT: AtomicU64 = AtomicU64::new(1);
  Arc::from(format!("task-{}", NEXT.fetch_add(1, Ordering::Relaxed)))
}

fn bench_msg_context(c: &mut Criterion) {
  // Stable per-actor inputs, established once at spawn time in the real runtime:
  // the delivery's correlation and the actor's own node id (an `Arc<str>` the
  // runtime shares — not re-allocates — into each per-message context).
  let correlation = CorrelationId::from("cid-bench");
  let node_id = Arc::<str>::from("node-bench");

  c.bench_function("context/per_message", |b| {
    b.iter(|| {
      // Reproduce `run_actor`'s `msg_ctx` exactly: `execution_id` from the
      // delivery's correlation (refcount bump), `node_id` shared from the
      // actor's stable id (refcount bump), `task_id` minted fresh (allocation).
      let ctx = ActorContext::new(
        black_box(correlation.as_arc()),
        black_box(Arc::clone(&node_id)),
        black_box(next_task_id()),
      );
      black_box(ctx)
    })
  });
}

criterion_group!(benches, bench_msg_context);
criterion_main!(benches);
