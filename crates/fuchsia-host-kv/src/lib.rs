use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// Execution-scoped key-value storage.
///
/// Each workflow execution gets isolated state. This trait is async
/// to support networked backends (e.g. Redis).
///
/// Implementations must be Send + Sync for use across async runtimes.
pub trait KvStore: Send + Sync {
    fn get(&self, key: &str) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>>;
    fn set(&mut self, key: &str, value: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
    fn delete(&mut self, key: &str) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// In-memory KV store backed by a HashMap.
///
/// Suitable for single-execution use or testing.
#[derive(Debug, Default)]
pub struct InMemoryKvStore {
    data: HashMap<String, String>,
}

impl InMemoryKvStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KvStore for InMemoryKvStore {
    fn get(&self, key: &str) -> Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        let value = self.data.get(key).cloned();
        Box::pin(async move { value })
    }

    fn set(&mut self, key: &str, value: String) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.data.insert(key.to_string(), value);
        Box::pin(async {})
    }

    fn delete(&mut self, key: &str) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.data.remove(key);
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_kv_store() {
        let mut store = InMemoryKvStore::new();

        assert_eq!(store.get("key").await, None);

        store.set("key", "value".to_string()).await;
        assert_eq!(store.get("key").await, Some("value".to_string()));

        store.set("key", "updated".to_string()).await;
        assert_eq!(store.get("key").await, Some("updated".to_string()));

        store.delete("key").await;
        assert_eq!(store.get("key").await, None);
    }
}
