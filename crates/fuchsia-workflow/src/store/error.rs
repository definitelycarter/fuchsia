use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkflowError {
  #[error("database: {0}")]
  Db(#[from] slate_db::DbError),
  #[error("serialization: {0}")]
  Serialization(String),
  #[error("workflow not found: {0}")]
  NotFound(String),
}
