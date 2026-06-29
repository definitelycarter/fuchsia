//! Benches the conditioning operators' async `handle` — the hot per-message
//! path. `passthrough` is the bare emit baseline; `dedup` adds the
//! compare-and-filter the conditioning path actually does. Each iteration drives
//! one async `handle` to completion via `block_on`, the same shape as the
//! `fuchsia-runtime` benches.

use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use fuchsia_actor::{ActorCapabilities, ActorConfig, ActorContext, ActorCreator, Emit, Message};
use fuchsia_actor_builtins::{DedupCreator, PassthroughCreator};

/// Discards emissions — keeps the bench on the operator, not a real sink.
struct Noop;
impl Emit for Noop {
  fn emit_to(&self, _port: &str, _msg: Message) {}
}

fn rt() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap()
}

fn ctx() -> ActorContext {
  ActorContext::new("exec", "node", "task")
}

fn bench_passthrough(c: &mut Criterion) {
  let rt = rt();
  let caps = ActorCapabilities::new().with_emit(Arc::new(Noop));
  let mut actor = PassthroughCreator
    .create(&ActorConfig::default(), &caps)
    .unwrap();
  let ctx = ctx();
  let msg = Message::json("reading", serde_json::json!(42));

  c.bench_function("builtins/passthrough_handle", |b| {
    b.iter(|| rt.block_on(actor.handle(&ctx, msg.clone())).unwrap())
  });
}

fn bench_dedup(c: &mut Criterion) {
  let rt = rt();
  let caps = ActorCapabilities::new().with_emit(Arc::new(Noop));
  let mut actor = DedupCreator.create(&ActorConfig::default(), &caps).unwrap();
  let ctx = ctx();
  // Alternate two distinct values so each message differs from the last and
  // exercises the *emit* branch (a repeat would exercise the drop branch).
  let a = Message::json("reading", serde_json::json!(1));
  let b = Message::json("reading", serde_json::json!(2));
  let mut toggle = false;

  c.bench_function("builtins/dedup_handle", |bch| {
    bch.iter(|| {
      let msg = if toggle { a.clone() } else { b.clone() };
      toggle = !toggle;
      rt.block_on(actor.handle(&ctx, msg)).unwrap();
    })
  });
}

criterion_group!(benches, bench_passthrough, bench_dedup);
criterion_main!(benches);
