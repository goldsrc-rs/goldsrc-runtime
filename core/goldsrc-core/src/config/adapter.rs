//! Bidirectional configuration binder bridging disk TOML files, in-memory domain models,
//! and GoldSrc engine console variables.

use goldsrc_api::cvar::ConfigModel;
use goldsrc_spi::engine::Engine;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Reactive configuration binder for managing disk persistence and CVAR synchronizations.
pub struct ConfigBinder<T: ConfigModel> {
    path: PathBuf,
    model: T,
    engine: Option<Arc<dyn Engine>>,
}

impl<T: ConfigModel + Serialize + DeserializeOwned + Default> ConfigBinder<T> {
    /// Creates a new configuration binder for the specified path and optional engine bridge.
    pub fn new(
        path: impl Into<PathBuf>,
        default_model: T,
        engine: Option<Arc<dyn Engine>>,
    ) -> Self {
        Self {
            path: path.into(),
            model: default_model,
            engine,
        }
    }

    /// Returns a reference to the active in-memory configuration model.
    #[inline]
    pub fn model(&self) -> &T {
        &self.model
    }

    /// Returns a mutable reference to the active in-memory configuration model.
    #[inline]
    pub fn model_mut(&mut self) -> &mut T {
        &mut self.model
    }

    /// Returns the target file path.
    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the configuration from disk, or generates and saves default template if missing.
    ///
    /// Automatically registers and synchronizes engine CVARs if an engine reference is present.
    pub fn load_or_create(&mut self) -> Result<(), std::io::Error> {
        if self.path.exists() {
            let content = fs::read_to_string(&self.path)?;
            match toml::from_str::<T>(&content) {
                Ok(parsed) => {
                    self.model = parsed;
                    if let Some(engine) = &self.engine {
                        self.model.register_cvars(engine.as_ref());
                        self.model.sync_to_cvars(engine.as_ref());
                    }
                }
                Err(err) => {
                    log::warn!(
                        target: goldsrc_api::consts::log_targets::CORE,
                        "Failed to parse TOML configuration at \"{}\": {err}. Preserving active defaults.",
                        self.path.display()
                    );
                }
            }
        } else {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent)?;
            }
            if let Some(engine) = &self.engine {
                self.model.register_cvars(engine.as_ref());
            }
            let toml_content = self.model.to_toml();
            fs::write(&self.path, toml_content)?;
        }

        Ok(())
    }

    /// Writes the current in-memory model to disk in documented TOML format.
    pub fn save_to_disk(&self) -> Result<(), std::io::Error> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = self.model.to_toml();
        fs::write(&self.path, content)
    }

    /// Exports GoldSrc console variable `.cfg` script bindings to the specified path.
    pub fn export_cvars_cfg(&self, path: impl AsRef<Path>) -> Result<(), std::io::Error> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = self.model.to_cvars();
        fs::write(path, content)
    }

    /// Synchronizes the in-memory model from current engine CVAR values.
    pub fn sync_from_cvars(&mut self) {
        if let Some(engine) = &self.engine {
            self.model.sync_from_cvars(engine.as_ref());
        }
    }

    /// Writes current in-memory model values into the engine's CVARs.
    pub fn sync_to_cvars(&self) {
        if let Some(engine) = &self.engine {
            self.model.sync_to_cvars(engine.as_ref());
        }
    }
}

/// External serialization adapter converting between TOML documents on disk and abstract `SettingTree`.
pub struct TomlSettingsAdapter;

impl TomlSettingsAdapter {
    /// Loads an abstract `SettingTree` from a TOML string via StateFS TOML ingestion.
    pub fn parse_str(content: &str) -> Result<goldsrc_api::setting::SettingTree, String> {
        let mut store = statefs_core::MemStore::new();
        statefs_codec_toml::ingest_toml(&mut store, content)
            .map_err(|e| format!("failed to ingest TOML configuration into StateFS: {e}"))?;

        let mut tree = goldsrc_api::setting::SettingTree::new();
        for (path, node) in store.find_glob("/**") {
            let key = path.segments().join(".");
            let val = match &node.value {
                statefs_core::Value::String(s) => s.clone(),
                statefs_core::Value::Int(i) => i.to_string(),
                statefs_core::Value::Float(f) => f.to_string(),
                statefs_core::Value::Bool(b) => b.to_string(),
                statefs_core::Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                other => format!("{other:?}"),
            };
            tree.insert(key, val);
        }
        Ok(tree)
    }

    /// Emits a documented TOML string from aggregate `Settings` schema and tree.
    pub fn emit_toml<S: goldsrc_api::setting::Settings>(settings: &S) -> String {
        let tree = settings.export_tree();
        let schema = S::schema();

        let mut out = String::with_capacity(1024);
        out.push_str("# Generated by GoldSrc.rs Settings Engine\n\n");

        for meta in schema {
            if !meta.description.is_empty() {
                out.push_str(&format!("# {}\n", meta.description));
            }
            let val = tree
                .get(meta.key)
                .cloned()
                .unwrap_or_else(|| meta.default_repr.to_string());
            // Format value: if boolean or number, print raw; otherwise quote string
            if val == "true" || val == "false" || val.parse::<f64>().is_ok() {
                out.push_str(&format!("{} = {}\n\n", meta.key, val));
            } else {
                out.push_str(&format!("{} = \"{}\"\n\n", meta.key, val));
            }
        }

        out
    }

    /// Loads settings from disk at `path`, or creates default file if missing.
    pub fn load_or_create<S: goldsrc_api::setting::Settings>(
        path: &Path,
        settings: &S,
    ) -> Result<(), std::io::Error> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            match Self::parse_str(&content) {
                Ok(tree) => {
                    if let Err(err) = settings.load_from_tree(&tree) {
                        log::warn!(
                            target: goldsrc_api::consts::log_targets::CORE,
                            "Failed to apply settings from \"{}\": {err}. Preserving active defaults.",
                            path.display()
                        );
                    }
                }
                Err(e) => {
                    log::warn!(
                        target: goldsrc_api::consts::log_targets::CORE,
                        "Failed to parse TOML settings at \"{}\": {e}. Preserving active defaults.",
                        path.display()
                    );
                }
            }
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let toml_content = Self::emit_toml(settings);
            fs::write(path, toml_content)?;
        }
        Ok(())
    }

    /// Saves settings to disk at `path`.
    pub fn save<S: goldsrc_api::setting::Settings>(
        path: &Path,
        settings: &S,
    ) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let toml_content = Self::emit_toml(settings);
        fs::write(path, toml_content)
    }
}

/// External console adapter binding aggregate `Settings` to engine console variables (CVars).
pub struct CvarSettingsAdapter;

impl CvarSettingsAdapter {
    /// Registers all settings in `Settings::schema()` as engine CVars with standard `ARCHIVE` flags.
    pub fn register_all<S: goldsrc_api::setting::Settings>(
        engine: &dyn goldsrc_api::cvar::CvarEngine,
        _settings: &S,
    ) {
        let schema = S::schema();
        for meta in schema {
            let cvar_name = meta.key.replace('.', "_");
            engine.cvar_register(
                &cvar_name,
                meta.default_repr,
                goldsrc_api::cvar::CvarFlags::ARCHIVE,
            );
        }
    }

    /// Synchronizes engine CVar values into the aggregate `Settings` via `CvarStorageAdapter`.
    pub fn sync_from_cvars<S: goldsrc_api::setting::Settings>(
        engine: &dyn goldsrc_api::cvar::CvarEngine,
        settings: &S,
    ) -> Result<(), goldsrc_api::setting::SettingError> {
        let schema = S::schema();
        let mut adapter = crate::config::cvar::CvarStorageAdapter::new();
        let mut tree = goldsrc_api::setting::SettingTree::new();

        for meta in schema {
            let cvar_name = meta.key.replace('.', "_");
            if let Some(str_val) = engine.cvar_get_string(&cvar_name) {
                let path = adapter.cvar_name_to_path(&cvar_name);
                let _ = statefs_config::storage::port::ConfigStorageMut::insert(
                    &mut adapter,
                    &path,
                    statefs_core::Value::from(str_val.as_str()),
                );
                tree.insert(meta.key.to_string(), str_val);
            }
        }

        settings.load_from_tree(&tree)
    }

    /// Synchronizes current aggregate `Settings` values into engine CVars via `CvarStorageAdapter`.
    pub fn sync_to_cvars<S: goldsrc_api::setting::Settings>(
        engine: &dyn goldsrc_api::cvar::CvarEngine,
        settings: &S,
    ) {
        let tree = settings.export_tree();
        let mut adapter = crate::config::cvar::CvarStorageAdapter::new();

        for (k, v) in tree {
            let cvar_name = k.replace('.', "_");
            let path = adapter.cvar_name_to_path(&cvar_name);
            let _ = statefs_config::storage::port::ConfigStorageMut::insert(
                &mut adapter,
                &path,
                statefs_core::Value::from(v.as_str()),
            );
            engine.cvar_set_string(&cvar_name, &v);
        }
    }

    /// Emits a `.cfg` file format string representing console variable commands.
    pub fn emit_cfg<S: goldsrc_api::setting::Settings>(settings: &S) -> String {
        let tree = settings.export_tree();
        let schema = S::schema();

        let mut out = String::with_capacity(1024);
        out.push_str("// Generated by GoldSrc.rs CVar Adapter\n\n");

        for meta in schema {
            let cvar_name = meta.key.replace('.', "_");
            let val = tree
                .get(meta.key)
                .cloned()
                .unwrap_or_else(|| meta.default_repr.to_string());
            if !meta.description.is_empty() {
                out.push_str(&format!("{cvar_name} \"{val}\" // {}\n", meta.description));
            } else {
                out.push_str(&format!("{cvar_name} \"{val}\"\n"));
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goldsrc_api::cvar::CvarEngine;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
    struct MockPluginConfig {
        pub tag: String,
        pub bonus_hp: i32,
        pub enabled: bool,
    }

    impl ConfigModel for MockPluginConfig {
        fn to_toml(&self) -> String {
            format!(
                "# Bonus health on spawn\nbonus_hp = {}\n# Chat tag\ntag = \"{}\"\n# Plugin switch\nenabled = {}\n",
                self.bonus_hp, self.tag, self.enabled
            )
        }

        fn to_cvars(&self) -> String {
            format!(
                "test_bonus_hp \"{}\" // Bonus HP\ntest_tag \"{}\" // Tag\ntest_enabled \"{}\" // Switch\n",
                self.bonus_hp,
                self.tag,
                if self.enabled { 1 } else { 0 }
            )
        }

        fn register_cvars(&self, engine: &dyn CvarEngine) {
            engine.cvar_register(
                "test_bonus_hp",
                &self.bonus_hp.to_string(),
                goldsrc_api::cvar::CvarFlags::ARCHIVE,
            );
            engine.cvar_register("test_tag", &self.tag, goldsrc_api::cvar::CvarFlags::ARCHIVE);
            engine.cvar_register(
                "test_enabled",
                if self.enabled { "1" } else { "0" },
                goldsrc_api::cvar::CvarFlags::ARCHIVE,
            );
        }

        fn sync_from_cvars(&mut self, engine: &dyn CvarEngine) {
            self.bonus_hp = engine.cvar_get_float("test_bonus_hp") as i32;
            if let Some(t) = engine.cvar_get_string("test_tag") {
                self.tag = t;
            }
            self.enabled = engine.cvar_get_float("test_enabled") > 0.0;
        }

        fn sync_to_cvars(&self, engine: &dyn CvarEngine) {
            engine.cvar_set_float("test_bonus_hp", self.bonus_hp as f32);
            engine.cvar_set_string("test_tag", &self.tag);
            engine.cvar_set_float("test_enabled", if self.enabled { 1.0 } else { 0.0 });
        }
    }

    #[test]
    fn test_config_binder_load_or_create_and_export() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("goldsrc_test_cfg_{unique}"));
        let cfg_path = temp_dir.join("test_plugin.toml");

        let default_cfg = MockPluginConfig {
            bonus_hp: 25,
            tag: "VIP".to_string(),
            enabled: true,
        };

        let mut binder = ConfigBinder::new(&cfg_path, default_cfg.clone(), None);
        assert!(!cfg_path.exists());

        binder.load_or_create().unwrap();
        assert!(cfg_path.exists());

        // Verify disk content has comments and keys
        let on_disk = fs::read_to_string(&cfg_path).unwrap();
        assert!(on_disk.contains("# Bonus health on spawn"));
        assert!(on_disk.contains("bonus_hp = 25"));

        // Verify cvar export
        let cvars_path = temp_dir.join("test_cvars.cfg");
        binder.export_cvars_cfg(&cvars_path).unwrap();
        let cvar_content = fs::read_to_string(&cvars_path).unwrap();
        assert!(cvar_content.contains("test_bonus_hp \"25\" // Bonus HP"));

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

    use goldsrc_api::setting::Settings;

    struct MockEngineCvarStore {
        cvars: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    }

    impl MockEngineCvarStore {
        fn new() -> Self {
            Self {
                cvars: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            }
        }
    }

    impl goldsrc_api::cvar::CvarEngine for MockEngineCvarStore {
        fn cvar_register(
            &self,
            name: &str,
            default_val: &str,
            _flags: goldsrc_api::cvar::CvarFlags,
        ) -> bool {
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

    struct SampleDeclarativeSettings {
        pub enabled: goldsrc_api::setting::Setting<bool>,
        pub max_speed: goldsrc_api::setting::Setting<i32>,
    }

    impl goldsrc_api::setting::Settings for SampleDeclarativeSettings {
        fn schema() -> Vec<goldsrc_api::setting::SettingMeta> {
            vec![
                goldsrc_api::setting::SettingMeta {
                    key: "player.speed_boost",
                    default_repr: "true",
                    description: "Enables player speed boost",
                    section: Some("movement"),
                },
                goldsrc_api::setting::SettingMeta {
                    key: "player.max_speed",
                    default_repr: "320",
                    description: "Maximum movement speed clamp",
                    section: Some("movement"),
                },
            ]
        }

        fn load_from_tree(
            &self,
            tree: &goldsrc_api::setting::SettingTree,
        ) -> Result<(), goldsrc_api::setting::SettingError> {
            if let Some(val) = tree.get("player.speed_boost") {
                let _ = self.enabled.set(val.parse().unwrap());
            }
            if let Some(val) = tree.get("player.max_speed") {
                let _ = self.max_speed.set(val.parse().unwrap());
            }
            Ok(())
        }

        fn export_tree(&self) -> goldsrc_api::setting::SettingTree {
            let mut tree = goldsrc_api::setting::SettingTree::new();
            tree.insert(
                "player.speed_boost".to_string(),
                self.enabled.get().to_string(),
            );
            tree.insert(
                "player.max_speed".to_string(),
                self.max_speed.get().to_string(),
            );
            tree
        }
    }

    #[test]
    fn test_toml_and_cvar_settings_adapters() {
        let settings = SampleDeclarativeSettings {
            enabled: goldsrc_api::setting::Setting::new(
                goldsrc_api::setting::SettingMeta {
                    key: "player.speed_boost",
                    default_repr: "true",
                    description: "Enables player speed boost",
                    section: Some("movement"),
                },
                true,
                goldsrc_api::setting::SettingBounds::None,
            ),
            max_speed: goldsrc_api::setting::Setting::new(
                goldsrc_api::setting::SettingMeta {
                    key: "player.max_speed",
                    default_repr: "320",
                    description: "Maximum movement speed clamp",
                    section: Some("movement"),
                },
                320,
                goldsrc_api::setting::SettingBounds::None,
            ),
        };

        // 1. Test TOML emit and parse
        let toml_str = TomlSettingsAdapter::emit_toml(&settings);
        assert!(toml_str.contains("# Enables player speed boost"));
        assert!(toml_str.contains("player.speed_boost = true"));
        assert!(toml_str.contains("player.max_speed = 320"));

        let incoming_toml = r#"
[player]
speed_boost = false
max_speed = 400
"#;
        let parsed_tree = TomlSettingsAdapter::parse_str(incoming_toml).unwrap();
        assert_eq!(parsed_tree.get("player.speed_boost").unwrap(), "false");
        assert_eq!(parsed_tree.get("player.max_speed").unwrap(), "400");

        settings.load_from_tree(&parsed_tree).unwrap();
        assert!(!settings.enabled.get());
        assert_eq!(settings.max_speed.get(), 400);

        // 2. Test CVars sync
        let mock_engine = MockEngineCvarStore::new();
        CvarSettingsAdapter::register_all(&mock_engine, &settings);
        assert_eq!(
            mock_engine.cvar_get_string("player_speed_boost").as_deref(),
            Some("true")
        );

        // Sync from settings to cvars
        CvarSettingsAdapter::sync_to_cvars(&mock_engine, &settings);
        assert_eq!(
            mock_engine.cvar_get_string("player_speed_boost").as_deref(),
            Some("false")
        );
        assert_eq!(
            mock_engine.cvar_get_string("player_max_speed").as_deref(),
            Some("400")
        );

        // Modify in engine cvar and sync back
        mock_engine.cvar_set_string("player_max_speed", "500");
        CvarSettingsAdapter::sync_from_cvars(&mock_engine, &settings).unwrap();
        assert_eq!(settings.max_speed.get(), 500);

        // Check emit cfg
        let cfg_str = CvarSettingsAdapter::emit_cfg(&settings);
        assert!(cfg_str.contains("player_max_speed \"500\" // Maximum movement speed clamp"));
    }
}
