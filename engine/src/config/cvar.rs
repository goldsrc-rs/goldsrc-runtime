//! GoldSrc Engine CVAR Storage Adapter.
//!
//! Implements [`statefs_config::ConfigStoragePort`] and [`statefs_config::ConfigStorageMut`]
//! for the GoldSrc engine CVAR subsystem according to Sewing Machine Architecture (SMA).
//!
//! ### Architectural Role (SMA Triad)
//! - **Provider**: GoldSrc Game Engine C-ABI memory (`cvar_t*` and CvarPort/Engine SPI).
//! - **Adapter**: [`CvarStorageAdapter`] bridging engine CVARs and hierarchical StateFS paths.
//! - **Port**: [`statefs_config::ConfigStoragePort`] / [`statefs_config::ConfigStorageMut`].

use goldsrc_spi::cvar::{CvarFlags, CvarPort};
use statefs_config::ConfigError;
use statefs_config::storage::port::{ConfigStorageMut, ConfigStoragePort};
use statefs_core::{MemStore, Node, Path, Store, Value};
use std::sync::Arc;

/// Storage adapter bridging the GoldSrc engine CVAR memory space into StateFS configuration hierarchy.
///
/// Converts between engine flat names (e.g. `sv_gravity`, `amx_speed_boost`) and StateFS
/// hierarchical paths (e.g. `/cvar/sv/gravity` or `/cvar/amx_speed_boost`).
///
/// Keeps an in-memory cached [`MemStore`] mirror for zero-allocation sub-nanosecond queries
/// and synchronization with the engine.
pub struct CvarStorageAdapter {
    engine: Option<Arc<dyn CvarPort>>,
    store: MemStore,
    prefix: String,
}

impl CvarStorageAdapter {
    /// Creates a new `CvarStorageAdapter` without an active engine bridge (in-memory standalone mode).
    pub fn new() -> Self {
        Self {
            engine: None,
            store: MemStore::new(),
            prefix: "/cvar".to_string(),
        }
    }

    /// Creates a new `CvarStorageAdapter` bound to the engine's CVAR provider.
    pub fn with_engine(engine: Arc<dyn CvarPort>) -> Self {
        Self {
            engine: Some(engine),
            store: MemStore::new(),
            prefix: "/cvar".to_string(),
        }
    }

    /// Sets the root path prefix for CVARs in the StateFS hierarchy (default: `"/cvar"`).
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Returns the root path prefix.
    #[inline]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Borrows the internal StateFS [`MemStore`].
    #[inline]
    pub fn store(&self) -> &MemStore {
        &self.store
    }

    /// Mutably borrows the internal StateFS [`MemStore`].
    #[inline]
    pub fn store_mut(&mut self) -> &mut MemStore {
        &mut self.store
    }

    /// Converts a StateFS path into an engine CVAR name.
    ///
    /// Strips the adapter prefix and converts separators into underscores or keeps raw name.
    /// E.g. `/cvar/sv_gravity` -> `sv_gravity`, `/cvar/player/speed` -> `player_speed`.
    pub fn path_to_cvar_name(&self, path: &Path) -> String {
        let prefix_path = Path::parse(&self.prefix);
        let target_path = if let Some(stripped) = path.strip_prefix(&prefix_path) {
            stripped
        } else {
            path.clone()
        };
        target_path.segments().join("_")
    }

    /// Converts an engine CVAR name into a StateFS path.
    pub fn cvar_name_to_path(&self, cvar_name: &str) -> Path {
        let clean_prefix = self.prefix.trim_end_matches('/');
        let full = format!("{clean_prefix}/{cvar_name}");
        Path::parse(&full)
    }

    /// Synchronizes a specific CVAR from the game engine into the internal StateFS cache.
    pub fn sync_cvar_from_engine(&mut self, cvar_name: &str) -> bool {
        let Some(engine) = &self.engine else {
            return false;
        };

        if let Some(val_str) = engine.cvar_get_string(cvar_name) {
            let path = self.cvar_name_to_path(cvar_name);
            let value = parse_cvar_value(&val_str);
            let _ = self.store.insert(&path, value);
            true
        } else {
            false
        }
    }

    /// Synchronizes all cached CVARs to the game engine provider.
    pub fn sync_all_to_engine(&self) {
        let Some(engine) = &self.engine else {
            return;
        };

        let pattern = format!("{}/*", self.prefix.trim_end_matches('/'));
        for (path, node) in self.store.find_glob(&pattern) {
            let cvar_name = self.path_to_cvar_name(&path);
            let val_str = node_value_to_string(&node.value);
            engine.cvar_set_string(&cvar_name, &val_str);
        }
    }

    /// Registers a CVAR in the game engine with default value and flags, caching it in StateFS.
    pub fn register_cvar(&mut self, cvar_name: &str, default_val: &str, flags: CvarFlags) -> bool {
        let path = self.cvar_name_to_path(cvar_name);
        let val = parse_cvar_value(default_val);
        let _ = self.store.insert(&path, val);

        if let Some(engine) = &self.engine {
            engine.cvar_register(cvar_name, default_val, flags)
        } else {
            true
        }
    }
}

impl Default for CvarStorageAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigStoragePort for CvarStorageAdapter {
    fn get(&self, path: &Path) -> Option<&Node> {
        self.store.get(path)
    }

    fn find_glob<'a>(&'a self, pattern: &str) -> Box<dyn Iterator<Item = (Path, &'a Node)> + 'a> {
        Box::new(self.store.find_glob(pattern).into_iter())
    }
}

impl ConfigStorageMut for CvarStorageAdapter {
    fn insert(&mut self, path: &Path, value: Value) -> Result<(), ConfigError> {
        if let Some(engine) = &self.engine {
            let cvar_name = self.path_to_cvar_name(path);
            let val_str = node_value_to_string(&value);
            engine.cvar_set_string(&cvar_name, &val_str);
        }
        self.store.insert(path, value)?;
        Ok(())
    }

    fn insert_node(&mut self, path: &Path, node: Node) -> Result<(), ConfigError> {
        if let Some(engine) = &self.engine {
            let cvar_name = self.path_to_cvar_name(path);
            let val_str = node_value_to_string(&node.value);
            engine.cvar_set_string(&cvar_name, &val_str);
        }
        self.store.insert_node(path, node)?;
        Ok(())
    }

    fn remove(&mut self, path: &Path) -> Result<bool, ConfigError> {
        Ok(self.store.remove(path)?.is_some())
    }
}

fn parse_cvar_value(raw: &str) -> Value {
    if let Ok(i) = raw.parse::<i64>() {
        Value::from(i)
    } else if let Ok(f) = raw.parse::<f64>() {
        Value::from(f)
    } else if raw == "true" {
        Value::from(true)
    } else if raw == "false" {
        Value::from(false)
    } else {
        Value::from(raw)
    }
}

fn node_value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        _ => format!("{value:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockEngine {
        cvars: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    }

    impl MockEngine {
        fn new() -> Self {
            Self {
                cvars: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            }
        }
    }

    impl CvarPort for MockEngine {
        fn cvar_register(&self, name: &str, default_val: &str, _flags: CvarFlags) -> bool {
            let mut lock = self.cvars.lock().unwrap();
            lock.insert(name.to_string(), default_val.to_string());
            true
        }

        fn cvar_get_string(&self, name: &str) -> Option<String> {
            let lock = self.cvars.lock().unwrap();
            lock.get(name).cloned()
        }

        fn cvar_set_string(&self, name: &str, val: &str) {
            let mut lock = self.cvars.lock().unwrap();
            lock.insert(name.to_string(), val.to_string());
        }

        fn cvar_get_float(&self, name: &str) -> f32 {
            self.cvar_get_string(name)
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(0.0)
        }

        fn cvar_set_float(&self, name: &str, val: f32) {
            self.cvar_set_string(name, &val.to_string());
        }
    }

    #[test]
    fn test_cvar_storage_adapter_crud_and_engine_sync() {
        let engine = Arc::new(MockEngine::new());
        let mut adapter = CvarStorageAdapter::with_engine(engine.clone());

        // 1. Register CVAR
        assert!(adapter.register_cvar("sv_gravity", "800", CvarFlags::SERVER));
        assert_eq!(engine.cvar_get_string("sv_gravity").as_deref(), Some("800"));

        // 2. Read from adapter port
        let path = Path::parse("/cvar/sv_gravity");
        let node = adapter.get(&path).expect("Node must exist in StateFS");
        assert_eq!(node.value, Value::from(800));

        // 3. Mutate through ConfigStorageMut
        adapter
            .insert(&path, Value::from(400))
            .expect("insert must succeed");
        assert_eq!(engine.cvar_get_string("sv_gravity").as_deref(), Some("400"));
        assert_eq!(adapter.get(&path).unwrap().value, Value::from(400));

        // 4. Update in engine and sync back
        engine.cvar_set_string("sv_gravity", "600");
        assert!(adapter.sync_cvar_from_engine("sv_gravity"));
        assert_eq!(adapter.get(&path).unwrap().value, Value::from(600));
    }
}
