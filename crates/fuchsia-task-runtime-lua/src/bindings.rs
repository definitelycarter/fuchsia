//! Wire shared host capabilities into Lua globals.
//!
//! Registers `kv`, `config`, and `log` tables on a Lua state so that
//! Lua task scripts can call host services using the same API surface
//! as the WIT interface.

use std::sync::Arc;

use fuchsia_host_config::ConfigHost;
use fuchsia_host_kv::KvStore;
use fuchsia_host_log::{LogHost, LogLevel};
use fuchsia_task_runtime::Capabilities;
use mlua::{Lua, Result as LuaResult};
use tokio::sync::Mutex;

/// Register all capability bindings on the Lua state.
pub(crate) fn register_all(lua: &Lua, capabilities: &Capabilities) -> LuaResult<()> {
    register_kv(lua, Arc::clone(&capabilities.kv))?;
    register_config(lua, Arc::clone(&capabilities.config))?;
    register_log(lua, Arc::clone(&capabilities.log))?;
    Ok(())
}

/// Register `kv` table with `get`, `set`, `delete` functions.
fn register_kv(lua: &Lua, kv: Arc<Mutex<dyn KvStore>>) -> LuaResult<()> {
    let table = lua.create_table()?;

    // kv.get(key) -> string|nil
    let kv_get = Arc::clone(&kv);
    table.set(
        "get",
        lua.create_function(move |_, key: String| {
            let result = futures::executor::block_on(async {
                let store = kv_get.lock().await;
                store.get(&key).await
            });
            Ok(result)
        })?,
    )?;

    // kv.set(key, value)
    let kv_set = Arc::clone(&kv);
    table.set(
        "set",
        lua.create_function(move |_, (key, value): (String, String)| {
            futures::executor::block_on(async {
                let mut store = kv_set.lock().await;
                store.set(&key, value).await;
            });
            Ok(())
        })?,
    )?;

    // kv.delete(key)
    let kv_del = Arc::clone(&kv);
    table.set(
        "delete",
        lua.create_function(move |_, key: String| {
            futures::executor::block_on(async {
                let mut store = kv_del.lock().await;
                store.delete(&key).await;
            });
            Ok(())
        })?,
    )?;

    lua.globals().set("kv", table)?;
    Ok(())
}

/// Register `config` table with `get` function.
fn register_config(lua: &Lua, config: Arc<dyn ConfigHost>) -> LuaResult<()> {
    let table = lua.create_table()?;

    // config.get(key) -> string|nil
    table.set(
        "get",
        lua.create_function(move |_, key: String| {
            let result = config.get(&key).map(|s| s.to_string());
            Ok(result)
        })?,
    )?;

    lua.globals().set("config", table)?;
    Ok(())
}

/// Register `log` table with `log` function.
fn register_log(lua: &Lua, log: Arc<dyn LogHost>) -> LuaResult<()> {
    let table = lua.create_table()?;

    // log.log(level, message)
    table.set(
        "log",
        lua.create_function(move |_, (level_str, message): (String, String)| {
            let level = match level_str.as_str() {
                "trace" => LogLevel::Trace,
                "debug" => LogLevel::Debug,
                "info" => LogLevel::Info,
                "warn" => LogLevel::Warn,
                "error" => LogLevel::Error,
                _ => LogLevel::Info,
            };
            log.log(level, &message);
            Ok(())
        })?,
    )?;

    lua.globals().set("log", table)?;
    Ok(())
}
