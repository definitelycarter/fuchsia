use std::sync::Arc;

use fuchsia_host_config::ConfigHost;
use fuchsia_host_kv::KvStore;
use fuchsia_host_log::{LogHost, LogLevel};
use fuchsia_task_runtime::Capabilities;
use tokio::sync::Mutex;
use wasmtime::component::HasData;
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

// Generate bindings for the task-component WIT world.
wasmtime::component::bindgen!({
    path: "../../wit",
    world: "fuchsia:platform/task-component@0.1.0",
    exports: { default: async },
});

/// Wasmtime store state that delegates host imports to shared capabilities.
pub(crate) struct WasmTaskState {
    pub kv: Arc<Mutex<dyn KvStore>>,
    pub config: Arc<dyn ConfigHost>,
    pub log: Arc<dyn LogHost>,
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasmTaskState {
    pub fn from_capabilities(capabilities: &Capabilities) -> Self {
        let wasi = WasiCtxBuilder::new().build();
        Self {
            kv: Arc::clone(&capabilities.kv),
            config: Arc::clone(&capabilities.config),
            log: Arc::clone(&capabilities.log),
            wasi,
            table: ResourceTable::new(),
        }
    }
}

impl WasiView for WasmTaskState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl HasData for WasmTaskState {
    type Data<'a> = &'a mut Self;
}

// Wire WIT kv imports → shared KvStore capability.
impl fuchsia::kv::kv::Host for WasmTaskState {
    fn get(&mut self, key: String) -> Option<String> {
        futures::executor::block_on(async {
            let kv = self.kv.lock().await;
            kv.get(&key).await
        })
    }

    fn set(&mut self, key: String, value: String) {
        futures::executor::block_on(async {
            let mut kv = self.kv.lock().await;
            kv.set(&key, value).await;
        });
    }

    fn delete(&mut self, key: String) {
        futures::executor::block_on(async {
            let mut kv = self.kv.lock().await;
            kv.delete(&key).await;
        });
    }
}

// Wire WIT config imports → shared ConfigHost capability.
impl fuchsia::config::config::Host for WasmTaskState {
    fn get(&mut self, key: String) -> Option<String> {
        self.config.get(&key).map(|s| s.to_string())
    }
}

// Wire WIT log imports → shared LogHost capability.
impl fuchsia::log::log::Host for WasmTaskState {
    fn log(&mut self, level: fuchsia::log::log::Level, message: String) {
        let level = match level {
            fuchsia::log::log::Level::Trace => LogLevel::Trace,
            fuchsia::log::log::Level::Debug => LogLevel::Debug,
            fuchsia::log::log::Level::Info => LogLevel::Info,
            fuchsia::log::log::Level::Warn => LogLevel::Warn,
            fuchsia::log::log::Level::Error => LogLevel::Error,
        };
        self.log.log(level, &message);
    }
}

// Re-export generated types used by the executor.
pub(crate) use exports::fuchsia::task::task::Context;
