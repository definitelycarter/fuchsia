use std::sync::{Arc, Mutex};

use fuchsia_actor::{
  Actor, ActorCapabilities, ActorConfig, ActorContext, ActorCreator, ActorError, ActorId, Message,
};
use fuchsia_actor_builtins::PassthroughCreator;
use fuchsia_engine::Engine;
use fuchsia_provisioner::Provisioner;
use fuchsia_workflow::{BuiltinConfig, Edge, Node, NodeDefinition, NodeId, Workflow, WorkflowId};
use tokio::sync::Notify;

struct Recorder {
  recorded: Arc<Mutex<Vec<Message>>>,
  notify: Arc<Notify>,
}

impl Actor for Recorder {
  fn setup(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
  fn handle(&mut self, _ctx: &ActorContext, msg: Message) -> Result<(), ActorError> {
    self.recorded.lock().unwrap().push(msg);
    self.notify.notify_one();
    Ok(())
  }
  fn teardown(&mut self, _ctx: &ActorContext) -> Result<(), ActorError> {
    Ok(())
  }
}

struct RecorderCreator {
  recorded: Arc<Mutex<Vec<Message>>>,
  notify: Arc<Notify>,
}

impl ActorCreator for RecorderCreator {
  fn create(
    &self,
    _config: &ActorConfig,
    _caps: &ActorCapabilities,
  ) -> Result<Box<dyn Actor>, ActorError> {
    Ok(Box::new(Recorder {
      recorded: self.recorded.clone(),
      notify: self.notify.clone(),
    }))
  }
}

fn builtin(id: &str, name: &str) -> Node {
  Node {
    id: NodeId(id.to_owned()),
    definition: NodeDefinition::Builtin(BuiltinConfig {
      name: name.to_owned(),
      env: Default::default(),
      settings: Default::default(),
    }),
  }
}

#[tokio::test]
async fn register_workflow_builds_a_routable_graph_then_tears_it_down() {
  let recorded = Arc::new(Mutex::new(Vec::new()));
  let notify = Arc::new(Notify::new());

  let engine = Arc::new(Engine::new());
  engine.register("passthrough", PassthroughCreator).await;
  engine
    .register(
      "recorder",
      RecorderCreator {
        recorded: recorded.clone(),
        notify: notify.clone(),
      },
    )
    .await;
  let provisioner = Provisioner::new(engine.clone());

  // A stored workflow: a (passthrough) → b (recorder).
  let workflow = Workflow {
    id: WorkflowId::new(),
    name: "climate".to_owned(),
    nodes: vec![builtin("a", "passthrough"), builtin("b", "recorder")],
    edges: vec![Edge {
      from: NodeId("a".to_owned()),
      to: NodeId("b".to_owned()),
    }],
    created_at: 0,
    updated_at: 0,
  };
  let group = workflow.id.to_string();

  // Provision it, then push at its entrypoint — the message routes a → b.
  provisioner.register_workflow(&workflow).await.unwrap();
  engine
    .push(
      &ActorId::scoped(group.as_str(), "a"),
      Message::empty("ping"),
    )
    .unwrap();
  notify.notified().await;
  assert_eq!(recorded.lock().unwrap()[0].type_, "ping");

  // Tear the workflow down; its entrypoint is gone.
  provisioner.unregister_workflow(&workflow.id).await.unwrap();
  assert!(
    engine
      .push(&ActorId::scoped(group.as_str(), "a"), Message::empty("x"))
      .is_err()
  );
}
