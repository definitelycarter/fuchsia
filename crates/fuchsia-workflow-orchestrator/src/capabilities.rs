//! Capabilities factory for constructing per-node capabilities.
//!
//! KV store is created once per workflow execution (shared across all nodes).
//! Config, log, and HTTP hosts are constructed per-node with node-specific
//! policies and context.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use fuchsia_host_config::MapConfig;
use fuchsia_host_http::{HttpPolicy, ReqwestHttpHost};
use fuchsia_host_kv::{InMemoryKvStore, KvStore};
use fuchsia_host_log::TracingLogHost;
use fuchsia_task_runtime::Capabilities;

/// Builds `Capabilities` for a workflow execution.
///
/// Created once per `invoke()` call. The KV store is shared across all
/// nodes within that execution, while config/log/http are per-node.
pub struct CapabilitiesFactory {
    kv: Arc<Mutex<dyn KvStore>>,
}

impl Default for CapabilitiesFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilitiesFactory {
    /// Create a new factory with an in-memory KV store.
    pub fn new() -> Self {
        Self {
            kv: Arc::new(Mutex::new(InMemoryKvStore::new())),
        }
    }

    /// Create a factory with a custom KV store.
    pub fn with_kv(kv: Arc<Mutex<dyn KvStore>>) -> Self {
        Self { kv }
    }

    /// Build capabilities for a specific node execution.
    pub fn build(
        &self,
        execution_id: &str,
        node_id: &str,
        config_values: HashMap<String, String>,
        http_policy: HttpPolicy,
    ) -> Capabilities {
        Capabilities {
            kv: Arc::clone(&self.kv),
            config: Arc::new(MapConfig::new(config_values)),
            log: Arc::new(TracingLogHost::new(
                execution_id.to_string(),
                node_id.to_string(),
            )),
            http: Arc::new(ReqwestHttpHost::new(http_policy)),
        }
    }

    /// Build capabilities with default config and HTTP policy.
    pub fn build_default(&self, execution_id: &str, node_id: &str) -> Capabilities {
        self.build(
            execution_id,
            node_id,
            HashMap::new(),
            HttpPolicy::default(),
        )
    }
}
