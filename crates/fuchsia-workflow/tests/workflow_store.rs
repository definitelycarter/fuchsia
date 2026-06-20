use bson::doc;
use fuchsia_workflow::store::{
  self, ComponentConfig, Edge, NewWorkflow, Node, NodeDefinition, NodeId, Runtime,
};
use slate_db::DatabaseBuilder;
use slate_store::MemoryStore;

fn setup() -> slate_db::Database<MemoryStore> {
  let db = DatabaseBuilder::new().open(MemoryStore::new()).unwrap();
  let mut txn = db.begin(false).unwrap();
  store::init(&mut txn).unwrap();
  txn.commit().unwrap();
  db
}

fn component_node(id: &str, component: &str, runtime: Runtime) -> Node {
  Node {
    id: NodeId(id.to_string()),
    definition: NodeDefinition::Component(ComponentConfig {
      runtime,
      component: component.to_string(),
      settings: doc! { "threshold": 21 },
    }),
  }
}

fn new_workflow(name: &str) -> NewWorkflow {
  NewWorkflow {
    name: name.to_string(),
    nodes: vec![
      component_node("ingest", "mqtt-source", Runtime::Wasm),
      component_node("debounce", "debounce", Runtime::Lua),
    ],
    edges: vec![Edge {
      from: NodeId("ingest".to_string()),
      to: NodeId("debounce".to_string()),
    }],
  }
}

#[test]
fn create_and_get_round_trips() {
  let db = setup();

  let txn = db.begin(false).unwrap();
  let workflow = store::create(&txn, new_workflow("climate")).unwrap();
  txn.commit().unwrap();

  let txn = db.begin(true).unwrap();
  let fetched = store::get(&txn, workflow.id.clone())
    .unwrap()
    .expect("workflow should exist");

  assert_eq!(fetched.name, "climate");
  assert_eq!(fetched.created_at, fetched.updated_at);
  // The whole graph survives the BSON round trip, including the adjacently
  // tagged NodeDefinition enum and its opaque `settings` document.
  assert_eq!(fetched, workflow);
}

#[test]
fn node_definition_survives_bson() {
  let db = setup();

  let txn = db.begin(false).unwrap();
  let workflow = store::create(&txn, new_workflow("climate")).unwrap();
  txn.commit().unwrap();

  let txn = db.begin(true).unwrap();
  let fetched = store::get(&txn, workflow.id).unwrap().unwrap();

  let ingest = fetched.nodes.iter().find(|n| n.id.0 == "ingest").unwrap();
  match &ingest.definition {
    NodeDefinition::Component(cfg) => {
      assert_eq!(cfg.runtime, Runtime::Wasm);
      assert_eq!(cfg.component, "mqtt-source");
      assert_eq!(cfg.settings.get_i32("threshold").unwrap(), 21);
    }
    NodeDefinition::Builtin(_) => panic!("expected a component node"),
  }
}

#[test]
fn list_returns_all_workflows() {
  let db = setup();

  let txn = db.begin(false).unwrap();
  store::create(&txn, new_workflow("first")).unwrap();
  store::create(&txn, new_workflow("second")).unwrap();
  txn.commit().unwrap();

  let txn = db.begin(true).unwrap();
  let all = store::list(&txn).unwrap();
  let mut names: Vec<_> = all.iter().map(|w| w.name.as_str()).collect();
  names.sort();
  assert_eq!(names, vec!["first", "second"]);
}

#[test]
fn update_replaces_graph() {
  let db = setup();

  let txn = db.begin(false).unwrap();
  let workflow = store::create(&txn, new_workflow("original")).unwrap();
  txn.commit().unwrap();

  let created_at = workflow.created_at;
  let mut updated = workflow;
  updated.name = "renamed".to_string();
  updated.edges.clear();

  let txn = db.begin(false).unwrap();
  let result = store::update(&txn, updated).unwrap();
  txn.commit().unwrap();

  assert_eq!(result.name, "renamed");
  assert!(result.edges.is_empty());
  assert_eq!(result.created_at, created_at);
  assert!(result.updated_at >= created_at);
}

#[test]
fn delete_removes_workflow() {
  let db = setup();

  let txn = db.begin(false).unwrap();
  let workflow = store::create(&txn, new_workflow("to-delete")).unwrap();
  let id = workflow.id.clone();
  store::delete(&txn, id.clone()).unwrap();
  txn.commit().unwrap();

  let txn = db.begin(true).unwrap();
  let result = store::get(&txn, id).unwrap();
  assert!(result.is_none());
}
