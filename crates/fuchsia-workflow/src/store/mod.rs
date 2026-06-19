//! The persisted representation of a workflow: its definition (the node graph)
//! and Slate-backed CRUD. Mirrors `the entity crate`'s `store` module.
//!
//! Placement is fixed: every workflow definition lives in one collection inside
//! the durable config column family. Workflow definitions are always durable,
//! so — unlike entities — they carry no durability policy. The caller owns the
//! transaction; this module owns the cf/collection within it.

mod error;
mod node;
mod workflow;

use std::time::{SystemTime, UNIX_EPOCH};

use bson::{doc, oid::ObjectId};
use slate_db::{CollectionConfig, DatabaseTransaction};
use slate_query::FindOptions;
use slate_store::Store;

pub use error::WorkflowError;
pub use node::{
  BuiltinConfig, ComponentConfig, Edge, Node, NodeDefinition, NodeId, Runtime, Trigger,
};
pub use workflow::{NewWorkflow, Workflow, WorkflowId};

const CF: &str = "config";
const COLLECTION: &str = "workflow";

/// Create the workflow collection in the config CF. Call once per database
/// (startup, and test setup) before any other operation.
pub fn init<S: Store>(txn: &mut DatabaseTransaction<'_, S>) -> Result<(), WorkflowError> {
  txn.create_collection(&CollectionConfig {
    name: COLLECTION.to_string(),
    cf: CF.to_string(),
    pk_path: "_id".to_string(),
    ttl_path: "ttl".to_string(),
  })?;
  Ok(())
}

pub fn create<S: Store>(
  txn: &DatabaseTransaction<'_, S>,
  new: NewWorkflow,
) -> Result<Workflow, WorkflowError> {
  let now = now_millis();
  let workflow = Workflow {
    id: WorkflowId(ObjectId::new()),
    name: new.name,
    nodes: new.nodes,
    edges: new.edges,
    created_at: now,
    updated_at: now,
  };
  txn.insert_one(CF, COLLECTION, &workflow)?.drain()?;
  Ok(workflow)
}

pub fn get<S: Store>(
  txn: &DatabaseTransaction<'_, S>,
  id: WorkflowId,
) -> Result<Option<Workflow>, WorkflowError> {
  let options = FindOptions {
    take: Some(1),
    ..Default::default()
  };
  let cursor = txn.find(CF, COLLECTION, doc! { "_id": id.0 }, options)?;
  cursor
    .iter::<Workflow>()?
    .next()
    .transpose()
    .map_err(WorkflowError::Db)
}

pub fn update<S: Store>(
  txn: &DatabaseTransaction<'_, S>,
  mut workflow: Workflow,
) -> Result<Workflow, WorkflowError> {
  workflow.updated_at = now_millis();
  let oid = workflow.id.0;
  txn
    .replace_one(CF, COLLECTION, doc! { "_id": oid }, &workflow)?
    .drain()?;
  Ok(workflow)
}

pub fn delete<S: Store>(
  txn: &DatabaseTransaction<'_, S>,
  id: WorkflowId,
) -> Result<(), WorkflowError> {
  txn
    .delete_one(CF, COLLECTION, doc! { "_id": id.0 })?
    .drain()?;
  Ok(())
}

pub fn list<S: Store>(txn: &DatabaseTransaction<'_, S>) -> Result<Vec<Workflow>, WorkflowError> {
  let cursor = txn.find(CF, COLLECTION, doc! {}, FindOptions::default())?;
  cursor
    .iter::<Workflow>()?
    .map(|result| result.map_err(WorkflowError::Db))
    .collect()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_millis() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_millis() as i64)
    .unwrap_or(0)
}
