//! In-memory component registry for testing.
//!
//! Stores component bytes in memory and writes them to temp files
//! when resolved, so callers that read `wasm_path` from disk still work.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::RegistryError;
use crate::manifest::ComponentManifest;
use crate::registry::{ComponentRegistry, InstalledComponent};

/// In-memory component registry for testing.
///
/// Components are registered with `register()` and stored in memory.
/// On `get()`, the bytes are written to a temp file so that callers
/// reading `installed.wasm_path` from disk still work.
type ComponentEntry = (ComponentManifest, Vec<u8>);

pub struct InMemoryComponentRegistry {
    components: Mutex<HashMap<(String, String), ComponentEntry>>,
    temp_dir: PathBuf,
}

impl InMemoryComponentRegistry {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_dir = std::env::temp_dir().join(format!(
            "fuschia-test-registry-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(&temp_dir).expect("failed to create temp dir");
        Self {
            components: Mutex::new(HashMap::new()),
            temp_dir,
        }
    }

    /// Register a component with its raw bytes (e.g., Lua source or wasm binary).
    pub fn register(&self, name: &str, version: &str, bytes: Vec<u8>) {
        let manifest = ComponentManifest {
            name: name.to_string(),
            version: version.to_string(),
            description: String::new(),
            digest: format!("sha256:{:x}", fxhash(name, version)),
            capabilities: Default::default(),
            tasks: HashMap::new(),
        };

        // Write bytes to disk so resolve_component_bytes can read them.
        let dir = self.temp_dir.join(manifest.dir_name());
        std::fs::create_dir_all(&dir).expect("failed to create component dir");
        let wasm_path = dir.join("component.wasm");
        std::fs::write(&wasm_path, &bytes).expect("failed to write component bytes");

        self.components
            .lock()
            .unwrap()
            .insert((name.to_string(), version.to_string()), (manifest, bytes));
    }
}

impl Default for InMemoryComponentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ComponentRegistry for InMemoryComponentRegistry {
    async fn get(
        &self,
        name: &str,
        version: Option<&str>,
    ) -> Result<Option<InstalledComponent>, RegistryError> {
        let components = self.components.lock().unwrap();

        let version = version.unwrap_or("0.0.0");
        let key = (name.to_string(), version.to_string());

        match components.get(&key) {
            Some((manifest, _bytes)) => {
                let dir = self.temp_dir.join(manifest.dir_name());
                let wasm_path = dir.join("component.wasm");
                Ok(Some(InstalledComponent {
                    manifest: manifest.clone(),
                    wasm_path,
                    component_dir: dir,
                }))
            }
            None => Ok(None),
        }
    }

    async fn install(
        &self,
        _package_path: &std::path::Path,
    ) -> Result<InstalledComponent, RegistryError> {
        unimplemented!("InMemoryComponentRegistry does not support install")
    }

    async fn list(&self) -> Result<Vec<ComponentManifest>, RegistryError> {
        let components = self.components.lock().unwrap();
        Ok(components.values().map(|(m, _)| m.clone()).collect())
    }

    async fn remove(&self, _name: &str, _version: &str) -> Result<(), RegistryError> {
        unimplemented!("InMemoryComponentRegistry does not support remove")
    }
}

/// Simple hash for generating fake digests.
fn fxhash(name: &str, version: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    version.hash(&mut hasher);
    hasher.finish()
}
