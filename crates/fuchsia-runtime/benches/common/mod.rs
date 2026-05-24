#![allow(dead_code)]

use async_trait::async_trait;
use fuchsia_actor::{Actor, ActorError, Context, Emitter, Inbox, Message};
use fuchsia_runtime::{ActorRegistry, Edge, Graph, Node};
use serde_json::Value;

pub struct Passthrough;

#[async_trait]
impl Actor for Passthrough {
  async fn run(&self, mut inbox: Inbox, emit: Emitter, ctx: Context) -> Result<(), ActorError> {
    loop {
      tokio::select! {
          _ = ctx.cancelled() => return Ok(()),
          msg = inbox.recv() => match msg {
              Some(msg) => emit.send(msg).await?,
              None => return Ok(()),
          }
      }
    }
  }
}

pub struct Sink;

#[async_trait]
impl Actor for Sink {
  async fn run(&self, mut inbox: Inbox, _emit: Emitter, ctx: Context) -> Result<(), ActorError> {
    loop {
      tokio::select! {
          _ = ctx.cancelled() => return Ok(()),
          msg = inbox.recv() => match msg {
              Some(_) => {}
              None => return Ok(()),
          }
      }
    }
  }
}

pub fn registry() -> ActorRegistry {
  let mut reg = ActorRegistry::new();
  reg.register::<Passthrough, Value, _>("passthrough", |_| Passthrough);
  reg.register::<Sink, Value, _>("sink", |_| Sink);
  reg
}

/// Linear chain: entry passthrough → K-1 more passthroughs → sink.
/// Total node count = K + 1.
pub fn chain(k: usize) -> Graph {
  assert!(k >= 1, "chain must have at least one passthrough");
  let mut nodes = Vec::with_capacity(k + 1);
  let mut edges = Vec::with_capacity(k);

  for i in 0..k {
    nodes.push(Node {
      id: format!("n{i}"),
      actor: "passthrough".into(),
      config: Value::Null,
    });
  }
  nodes.push(Node {
    id: "sink".into(),
    actor: "sink".into(),
    config: Value::Null,
  });

  for i in 0..(k - 1) {
    edges.push(Edge {
      from: format!("n{i}"),
      to: format!("n{}", i + 1),
    });
  }
  edges.push(Edge {
    from: format!("n{}", k - 1),
    to: "sink".into(),
  });

  Graph {
    entry: "n0".into(),
    nodes,
    edges,
  }
}

/// Fan-out: one entry passthrough → `width` sink siblings.
pub fn fan_out(width: usize) -> Graph {
  assert!(width >= 1, "fan_out requires width >= 1");
  let mut nodes = Vec::with_capacity(width + 1);
  let mut edges = Vec::with_capacity(width);

  nodes.push(Node {
    id: "in".into(),
    actor: "passthrough".into(),
    config: Value::Null,
  });

  for i in 0..width {
    let id = format!("sink{i}");
    nodes.push(Node {
      id: id.clone(),
      actor: "sink".into(),
      config: Value::Null,
    });
    edges.push(Edge {
      from: "in".into(),
      to: id,
    });
  }

  Graph {
    entry: "in".into(),
    nodes,
    edges,
  }
}

pub fn bench_msg(i: u64) -> Message {
  Message::json("bench", serde_json::json!(i))
}
