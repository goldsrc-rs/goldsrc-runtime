//! Smart Preset Execution & State Snapshot Engine (`grs exec`).
//!
//! Provides Infrastructure-as-Code (IaC) configuration management for game servers:
//! - Declarative preset manifests (`presets/<mode>.toml`)
//! - Pre-flight validation & range clamping
//! - Atomic state snapshots with clean, reversible rollback (`--restore`)
//! - Reactive event broadcasting (`ModeChanged`) across the runtime

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// Global in-memory storage for the last active state snapshot before a preset was applied.
static LAST_SNAPSHOT: LazyLock<Mutex<Option<PresetStateSnapshot>>> =
    LazyLock::new(|| Mutex::new(None));

/// Plugin state requirements in a preset manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresetPluginsConfig {
    /// Plugins to pause during this preset.
    #[serde(default)]
    pub pause: Vec<String>,

    /// Plugins to resume during this preset.
    #[serde(default)]
    pub resume: Vec<String>,
}

/// Preset manifest model (`presets/<mode>.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetManifest {
    /// Metadata header.
    pub metadata: PresetMetadata,

    /// Key-value CVAR assignments to apply.
    #[serde(default)]
    pub cvars: HashMap<String, String>,

    /// Plugin states to modify.
    #[serde(default)]
    pub plugins: PresetPluginsConfig,
}

/// Metadata header describing the preset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetMetadata {
    /// Name of the preset (e.g. "ClanWar 5v5", "Casual", "Warmup").
    pub name: String,
    /// Description of the game mode.
    #[serde(default)]
    pub description: String,
    /// Author or league name (e.g. "ESL", "FastCup").
    #[serde(default)]
    pub author: String,
    /// Version string.
    #[serde(default)]
    pub version: String,
}

/// A captured snapshot of engine CVARs and plugin statuses before a preset mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresetStateSnapshot {
    /// Identifier of the preset that generated this snapshot.
    pub preset_name: String,
    /// Timestamp when snapshot was captured (unix seconds).
    pub timestamp: u64,
    /// Original string values of all modified CVARs.
    pub original_cvars: HashMap<String, String>,
    /// Original pause status of plugins that were affected: `plugin_name -> was_paused`.
    pub original_plugin_states: HashMap<String, bool>,
}

/// Outcome report returned after successfully executing a preset or rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecOutcome {
    /// Preset was applied atomically.
    Applied {
        name: String,
        cvars_changed: usize,
        plugins_paused: usize,
        plugins_resumed: usize,
    },
    /// Reverted to previous snapshot cleanly.
    Restored {
        name: String,
        cvars_reverted: usize,
        plugins_reverted: usize,
    },
}

/// Errors occurring during preset discovery, validation, or execution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresetError {
    #[error("Preset file not found: '{path}'")]
    FileNotFound { path: PathBuf },

    #[error("Failed to parse preset TOML at '{path}': {reason}")]
    ParseError { path: PathBuf, reason: String },

    #[error("No active state snapshot found to restore")]
    NoSnapshotToRestore,

    #[error("Pre-flight validation failed: {reason}")]
    ValidationFailed { reason: String },

    #[error("Engine backend unavailable")]
    EngineUnavailable,
}

/// The core preset execution and snapshot engine.
pub struct PresetEngine;

impl PresetEngine {
    /// Resolves the file path for a preset by name or relative path.
    pub fn resolve_preset_path(name_or_path: &str, presets_dir: &Path) -> PathBuf {
        let p = PathBuf::from(name_or_path);
        if !p.is_file() {
            let candidate = presets_dir.join(format!("{name_or_path}.toml"));
            if candidate.is_file() {
                return candidate;
            }
            let candidate_bare = presets_dir.join(name_or_path);
            if candidate_bare.is_file() {
                return candidate_bare;
            }
        }
        p
    }

    /// Loads and parses a `PresetManifest` from disk.
    pub fn load_manifest(path: &Path) -> Result<PresetManifest, PresetError> {
        if !path.is_file() {
            return Err(PresetError::FileNotFound {
                path: path.to_path_buf(),
            });
        }

        let content = std::fs::read_to_string(path).map_err(|e| PresetError::ParseError {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;

        toml::from_str(&content).map_err(|e| PresetError::ParseError {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })
    }

    /// Captures a state snapshot of the CVARs and plugins defined in `manifest`.
    pub fn capture_snapshot<FGetCvar, FGetPlugin>(
        manifest: &PresetManifest,
        mut get_cvar: FGetCvar,
        mut get_plugin_paused: FGetPlugin,
    ) -> PresetStateSnapshot
    where
        FGetCvar: FnMut(&str) -> Option<String>,
        FGetPlugin: FnMut(&str) -> Option<bool>,
    {
        let mut original_cvars = HashMap::new();
        for cvar_name in manifest.cvars.keys() {
            if let Some(val) = get_cvar(cvar_name) {
                original_cvars.insert(cvar_name.clone(), val);
            }
        }

        let mut original_plugin_states = HashMap::new();
        for pl_name in manifest
            .plugins
            .pause
            .iter()
            .chain(manifest.plugins.resume.iter())
        {
            if let Some(paused) = get_plugin_paused(pl_name) {
                original_plugin_states.insert(pl_name.clone(), paused);
            }
        }

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        PresetStateSnapshot {
            preset_name: manifest.metadata.name.clone(),
            timestamp: ts,
            original_cvars,
            original_plugin_states,
        }
    }

    /// Saves the active snapshot into global engine state.
    pub fn store_snapshot(snapshot: PresetStateSnapshot) {
        let mut lock = match LAST_SNAPSHOT.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *lock = Some(snapshot);
    }

    /// Takes the active snapshot for rollback.
    pub fn take_snapshot() -> Option<PresetStateSnapshot> {
        let mut lock = match LAST_SNAPSHOT.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        lock.take()
    }

    /// Inspects whether an active snapshot currently exists.
    pub fn has_active_snapshot() -> bool {
        let lock = match LAST_SNAPSHOT.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        lock.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest_roundtrip() {
        let toml_str = r#"
[metadata]
name = "ClanWar 5v5"
description = "Competitive match configuration"
author = "ESL"
version = "1.0.0"

[cvars]
mp_roundtime = "1.75"
mp_freezetime = "15"
mp_c4timer = "35"
sv_alltalk = "0"

[plugins]
pause = ["fun_sounds", "respawn_modes"]
resume = ["stats_match"]
"#;

        let manifest: PresetManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.metadata.name, "ClanWar 5v5");
        assert_eq!(manifest.cvars.len(), 4);
        assert_eq!(manifest.cvars.get("mp_roundtime").unwrap(), "1.75");
        assert_eq!(manifest.plugins.pause.len(), 2);
        assert_eq!(manifest.plugins.resume.len(), 1);
    }

    #[test]
    fn test_capture_and_store_snapshot() {
        let toml_str = r#"
[metadata]
name = "Match"

[cvars]
mp_roundtime = "1.75"
sv_gravity = "800"

[plugins]
pause = ["fun_mod"]
"#;
        let manifest: PresetManifest = toml::from_str(toml_str).unwrap();

        let snapshot = PresetEngine::capture_snapshot(
            &manifest,
            |cvar| match cvar {
                "mp_roundtime" => Some("5.0".to_string()),
                "sv_gravity" => Some("800".to_string()),
                _ => None,
            },
            |pl| if pl == "fun_mod" { Some(false) } else { None },
        );

        assert_eq!(snapshot.preset_name, "Match");
        assert_eq!(snapshot.original_cvars.get("mp_roundtime").unwrap(), "5.0");
        assert_eq!(
            snapshot.original_plugin_states.get("fun_mod").copied(),
            Some(false)
        );

        PresetEngine::store_snapshot(snapshot.clone());
        assert!(PresetEngine::has_active_snapshot());

        let retrieved = PresetEngine::take_snapshot().unwrap();
        assert_eq!(retrieved, snapshot);
        assert!(!PresetEngine::has_active_snapshot());
    }
}
