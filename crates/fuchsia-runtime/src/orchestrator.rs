use crate::graph::Graph;
use crate::registry::ActorRegistry;
use fuchsia_actor::{ActorError, Context, Emitter, Inbox, Message};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

const CHANNEL_BUFFER: usize = 32;

pub struct Orchestrator {
  registry: Arc<ActorRegistry>,
}

impl Orchestrator {
  pub fn new(registry: Arc<ActorRegistry>) -> Self {
    Self { registry }
  }

  #[tracing::instrument(
    name = "workflow.start",
    skip_all,
    fields(
      entry = %graph.entry,
      nodes = graph.nodes.len(),
      edges = graph.edges.len(),
    ),
  )]
  pub fn start(&self, graph: &Graph) -> Result<WorkflowHandle, ActorError> {
    let mut senders: HashMap<String, mpsc::Sender<Message>> = HashMap::new();
    let mut receivers: HashMap<String, mpsc::Receiver<Message>> = HashMap::new();

    for node in &graph.nodes {
      let (tx, rx) = mpsc::channel::<Message>(CHANNEL_BUFFER);
      senders.insert(node.id.clone(), tx);
      receivers.insert(node.id.clone(), rx);
    }

    if !senders.contains_key(&graph.entry) {
      return Err(ActorError::UnknownNode(graph.entry.clone()));
    }
    for edge in &graph.edges {
      if !senders.contains_key(&edge.from) {
        return Err(ActorError::UnknownNode(edge.from.clone()));
      }
      if !senders.contains_key(&edge.to) {
        return Err(ActorError::UnknownNode(edge.to.clone()));
      }
    }

    let cancel = CancellationToken::new();
    let mut join_handles: Vec<JoinHandle<Result<(), ActorError>>> = Vec::new();

    for node in &graph.nodes {
      let downstream: Vec<mpsc::Sender<Message>> = graph
        .edges_from(&node.id)
        .map(|edge| senders[&edge.to].clone())
        .collect();

      let emit = Emitter::new(downstream);
      let inbox = Inbox::new(receivers.remove(&node.id).ok_or_else(|| {
        ActorError::Other(format!("internal: receiver missing for node {}", node.id))
      })?);
      let actor = self
        .registry
        .instantiate(&node.actor, node.config.clone())?;
      let ctx = Context::new(node.id.clone(), cancel.clone());

      let span = tracing::info_span!(
        "actor",
        node = %node.id,
        kind = %node.actor,
      );

      let handle = tokio::spawn(
        async move {
          tracing::debug!("actor starting");
          let result = actor.run(inbox, emit, ctx).await;
          match &result {
            Ok(()) => tracing::debug!("actor exited"),
            Err(e) => tracing::error!(error = %e, "actor exited with error"),
          }
          result
        }
        .instrument(span),
      );
      join_handles.push(handle);
    }

    let entry_sender = senders
      .remove(&graph.entry)
      .ok_or_else(|| ActorError::Other("internal: entry sender missing".into()))?;
    // Drop the orchestrator's remaining sender clones; downstream channels
    // now only have the senders held inside upstream actors' emitters.
    drop(senders);

    tracing::info!("workflow started");

    Ok(WorkflowHandle {
      entry: Some(entry_sender),
      cancel,
      join_handles,
    })
  }
}

pub struct WorkflowHandle {
  entry: Option<mpsc::Sender<Message>>,
  cancel: CancellationToken,
  join_handles: Vec<JoinHandle<Result<(), ActorError>>>,
}

impl WorkflowHandle {
  /// Push a message into the workflow's entry node.
  #[tracing::instrument(name = "workflow.send", level = "trace", skip_all)]
  pub async fn send(&self, msg: Message) -> Result<(), ActorError> {
    let entry = self
      .entry
      .as_ref()
      .ok_or_else(|| ActorError::Other("entry already closed".into()))?;
    entry
      .send(msg)
      .await
      .map_err(|e| ActorError::Send(e.to_string()))
  }

  /// Trigger cancellation. All actors observing `ctx.cancelled()` will exit.
  pub fn cancel(&self) {
    tracing::debug!("workflow.cancel");
    self.cancel.cancel();
  }

  /// Close the entry channel and wait for every actor task to finish.
  /// Returns one result per actor, in spawn order.
  #[tracing::instrument(name = "workflow.join", skip_all, fields(actors = self.join_handles.len()))]
  pub async fn join(mut self) -> Vec<Result<(), ActorError>> {
    // Dropping the entry sender lets the entry actor's inbox drain and close,
    // which cascades to all downstreams.
    self.entry = None;

    let mut results = Vec::with_capacity(self.join_handles.len());
    for handle in self.join_handles.drain(..) {
      match handle.await {
        Ok(r) => results.push(r),
        Err(_) => results.push(Err(ActorError::Panic)),
      }
    }
    tracing::info!("workflow joined");
    results
  }
}
