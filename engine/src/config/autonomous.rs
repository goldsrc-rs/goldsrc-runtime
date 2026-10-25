//! Self-healing autonomous configuration engine.
//!
//! Provides zero-initial-config server deployment by auto-generating missing
//! default configuration templates on boot, with recursive deep-merge schema updates
//! that non-destructively preserve administrator edits and custom values.

use crate::paths::{BackendType, PathResolver};
use std::fs;
use std::path::{Path, PathBuf};

/// Recursively deep-merges missing keys from `defaults` into `target`.
///
/// Any key or table present in `target` is preserved untouched (non-destructive),
/// while any keys newly declared in `defaults` that do not yet exist in `target` are inserted.
///
/// Returns `true` if any missing keys were inserted.
pub fn deep_merge_toml(defaults: &toml::Value, target: &mut toml::Value) -> bool {
    let mut modified = false;

    if let (toml::Value::Table(def_table), toml::Value::Table(target_table)) = (defaults, target) {
        for (k, def_v) in def_table {
            if let Some(target_v) = target_table.get_mut(k) {
                if matches!(def_v, toml::Value::Table(_))
                    && matches!(target_v, toml::Value::Table(_))
                    && deep_merge_toml(def_v, target_v)
                {
                    modified = true;
                }
            } else {
                target_table.insert(k.clone(), def_v.clone());
                modified = true;
            }
        }
    }

    modified
}

/// Autonomous configuration engine managing Host, Bundle, and Plugin configuration files.
pub struct SelfHealingConfigEngine;

impl SelfHealingConfigEngine {
    /// Resolves the canonical path for a host configuration (`goldsrc.toml`).
    pub fn host_config_path(backend: BackendType) -> PathBuf {
        PathResolver::main_config_path(backend)
    }

    /// Resolves the canonical path for a bundle configuration (`configs/bundles/<name>.toml`).
    pub fn bundle_config_path(backend: BackendType, bundle_name: &str) -> PathBuf {
        PathResolver::existing_config_dir(backend)
            .join("bundles")
            .join(format!("{bundle_name}.toml"))
    }

    /// Resolves the canonical path for a plugin configuration (`configs/plugins/<name>.toml`).
    pub fn plugin_config_path(backend: BackendType, plugin_name: &str) -> PathBuf {
        PathResolver::existing_config_dir(backend)
            .join("plugins")
            .join(format!("{plugin_name}.toml"))
    }

    /// Ensures a configuration file exists on disk, auto-generating it if missing or
    /// performing a non-destructive deep merge if new schema keys are present.
    ///
    /// Returns the active configuration string.
    pub fn ensure_config_or_merge(
        path: &Path,
        default_toml: &str,
    ) -> Result<String, std::io::Error> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, default_toml)?;
            return Ok(default_toml.to_string());
        }

        let existing_content = fs::read_to_string(path)?;

        let Ok(default_val) = toml::from_str::<toml::Value>(default_toml) else {
            return Ok(existing_content);
        };

        let Ok(mut existing_val) = toml::from_str::<toml::Value>(&existing_content) else {
            return Ok(existing_content);
        };

        if deep_merge_toml(&default_val, &mut existing_val) {
            let merged_content = toml::to_string_pretty(&existing_val).unwrap_or(existing_content);
            fs::write(path, &merged_content)?;
            Ok(merged_content)
        } else {
            Ok(existing_content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deep_merge_preserves_existing_values_and_adds_missing() {
        let default_str = r#"
[server]
port = 27015
max_players = 32

[metrics]
enabled = true
interval = 60
"#;
        let admin_str = r#"
[server]
port = 27020
custom_cvar = "goldsrc"
"#;

        let default_val: toml::Value = toml::from_str(default_str).unwrap();
        let mut admin_val: toml::Value = toml::from_str(admin_str).unwrap();

        let modified = deep_merge_toml(&default_val, &mut admin_val);
        assert!(modified);

        let table = admin_val.as_table().unwrap();
        let server = table.get("server").unwrap().as_table().unwrap();
        // Preserved admin value
        assert_eq!(server.get("port").unwrap().as_integer(), Some(27020));
        // Preserved admin custom key
        assert_eq!(server.get("custom_cvar").unwrap().as_str(), Some("goldsrc"));
        // Added missing default key
        assert_eq!(server.get("max_players").unwrap().as_integer(), Some(32));

        // Added missing table
        let metrics = table.get("metrics").unwrap().as_table().unwrap();
        assert_eq!(metrics.get("enabled").unwrap().as_bool(), Some(true));
        assert_eq!(metrics.get("interval").unwrap().as_integer(), Some(60));
    }

    #[test]
    fn test_ensure_config_or_merge_file_creation() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("goldsrc_test_autocfg_{unique}"));
        let cfg_path = temp_dir.join("configs").join("plugins").join("test.toml");

        let default_content = "[plugin]\nenabled = true\n";
        let res =
            SelfHealingConfigEngine::ensure_config_or_merge(&cfg_path, default_content).unwrap();

        assert_eq!(res, default_content);
        assert!(cfg_path.is_file());

        // Now test second run without changes
        let res2 =
            SelfHealingConfigEngine::ensure_config_or_merge(&cfg_path, default_content).unwrap();
        assert_eq!(res2, default_content);

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
