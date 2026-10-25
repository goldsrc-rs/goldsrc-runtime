//! Global host and server configuration (`goldsrc.toml`).
//!
//! Provides defensive validation, clamping, and automatic fallback generation
//! for server administrators.

use crate::logging::LogConfig;
use crate::paths::{BackendType, PathResolver};
use goldsrc_api::consts::log_targets;
use serde::{Deserialize, Serialize};

/// Behavior when a loaded `.wasm` file is removed from the plugins directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFileDeleted {
    /// Keep the plugin executing in memory until explicitly unloaded.
    #[default]
    Keep,
    /// Automatically unload the plugin when the file is removed.
    Unload,
}

/// Core directory layout configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreConfig {
    /// Directory containing `.wasm` plugin files.
    pub plugins_dir: String,
    /// Directory containing `.toml` plugin configurations.
    pub configs_dir: String,
    /// Directory containing log files.
    pub logs_dir: String,
}

impl CoreConfig {
    pub fn default_for(backend: BackendType) -> Self {
        Self {
            plugins_dir: PathResolver::normalize(
                &PathResolver::framework_dir(backend).join("plugins"),
            ),
            configs_dir: PathResolver::normalize(
                &PathResolver::framework_dir(backend).join("configs"),
            ),
            logs_dir: PathResolver::normalize(&PathResolver::framework_dir(backend).join("logs")),
        }
    }
}

/// Hot-reload and file-system watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    /// Debounce delay in milliseconds to avoid reading partially written files.
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Policy for when a `.wasm` file is deleted from disk.
    #[serde(default)]
    pub on_file_deleted: OnFileDeleted,
    /// Whether automatic hot-reload on `.wasm` modification is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Whether to watch `.toml` plugin config changes.
    #[serde(default = "default_true")]
    pub watch_configs: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            debounce_ms: 500,
            on_file_deleted: OnFileDeleted::Keep,
            watch_configs: true,
        }
    }
}

/// WASM runtime resource limits and execution deadlines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Memory limit per plugin in megabytes.
    #[serde(default = "default_memory_limit_mb")]
    pub default_memory_limit_mb: u32,

    /// Maximum table elements allowed in the Wasmtime store.
    #[serde(default = "default_max_table_elements")]
    pub max_table_elements: u32,

    /// Epoch deadline ticks for `on_frame` callback (watchdog).
    #[serde(default = "default_frame_epoch")]
    pub frame_epoch_deadline: u64,

    /// Epoch deadline ticks for `on_event` callback.
    #[serde(default = "default_event_epoch")]
    pub event_epoch_deadline: u64,

    /// Epoch deadline ticks for `on_command` callback.
    #[serde(default = "default_command_epoch")]
    pub command_epoch_deadline: u64,

    /// Epoch deadline ticks for `on_load` callback.
    #[serde(default = "default_load_epoch")]
    pub load_epoch_deadline: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            default_memory_limit_mb: 64,
            max_table_elements: 10000,
            frame_epoch_deadline: 5,
            event_epoch_deadline: 10,
            command_epoch_deadline: 10,
            load_epoch_deadline: 50,
        }
    }
}

/// Global GoldSrc.rs system configuration model (`goldsrc.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    /// Core framework directories.
    pub core: CoreConfig,

    /// Logging subsystem configuration.
    #[serde(default)]
    pub logging: LogConfig,

    /// Plugins and hot-reload watcher settings.
    #[serde(default)]
    pub watcher: WatcherConfig,

    /// WASM runtime resource limits.
    #[serde(default)]
    pub runtime: RuntimeConfig,

    /// Console pipeline preprocessor settings.
    #[serde(default)]
    pub pipeline: crate::config::ConsolePipelineConfig,
}

pub const MIN_DEBOUNCE_MS: u64 = 50;
pub const MAX_DEBOUNCE_MS: u64 = 5000;
pub const DEFAULT_DEBOUNCE_MS: u64 = 500;

pub const MIN_MEMORY_LIMIT_MB: u32 = 16;
pub const MAX_MEMORY_LIMIT_MB: u32 = 512;
pub const DEFAULT_MEMORY_LIMIT_MB: u32 = 64;

pub const MIN_TABLE_ELEMENTS: u32 = 100;
pub const MAX_TABLE_ELEMENTS: u32 = 100_000;
pub const DEFAULT_MAX_TABLE_ELEMENTS: u32 = 10_000;

pub const MIN_FRAME_EPOCH_DEADLINE: u64 = 1;
pub const MAX_FRAME_EPOCH_DEADLINE: u64 = 100;
pub const DEFAULT_FRAME_EPOCH_DEADLINE: u64 = 5;

pub const DEFAULT_EVENT_EPOCH_DEADLINE: u64 = 10;
pub const DEFAULT_COMMAND_EPOCH_DEADLINE: u64 = 10;
pub const DEFAULT_LOAD_EPOCH_DEADLINE: u64 = 50;

fn default_true() -> bool {
    true
}
fn default_debounce_ms() -> u64 {
    DEFAULT_DEBOUNCE_MS
}
fn default_memory_limit_mb() -> u32 {
    DEFAULT_MEMORY_LIMIT_MB
}
fn default_max_table_elements() -> u32 {
    DEFAULT_MAX_TABLE_ELEMENTS
}
fn default_frame_epoch() -> u64 {
    DEFAULT_FRAME_EPOCH_DEADLINE
}
fn default_event_epoch() -> u64 {
    DEFAULT_EVENT_EPOCH_DEADLINE
}
fn default_command_epoch() -> u64 {
    DEFAULT_COMMAND_EPOCH_DEADLINE
}
fn default_load_epoch() -> u64 {
    DEFAULT_LOAD_EPOCH_DEADLINE
}

impl HostConfig {
    pub fn default_for(backend: BackendType) -> Self {
        Self {
            core: CoreConfig::default_for(backend),
            logging: LogConfig::default(),
            watcher: WatcherConfig::default(),
            runtime: RuntimeConfig::default(),
            pipeline: crate::config::ConsolePipelineConfig::default(),
        }
    }

    /// Loads the configuration for `backend` or creates sanitized defaults.
    pub fn load_or_create(backend: BackendType) -> Self {
        let path = PathResolver::main_config_path(backend);
        if let Ok(content) = std::fs::read_to_string(&path) {
            match toml::from_str::<HostConfig>(&content) {
                Ok(mut cfg) => {
                    cfg.sanitize();
                    return cfg;
                }
                Err(e) => {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let backup_path = path.with_extension(format!("corrupted.{ts}.toml"));
                    let _ = std::fs::copy(&path, &backup_path);
                    log::error!(
                        target: log_targets::CORE,
                        "Failed to parse '{}': {e}. Preserved backup at '{}'. Running with safe in-memory defaults (original file NOT overwritten).",
                        path.display(),
                        backup_path.display()
                    );
                    let mut default_cfg = HostConfig::default_for(backend);
                    default_cfg.sanitize();
                    return default_cfg;
                }
            }
        }

        let mut default_cfg = HostConfig::default_for(backend);
        default_cfg.sanitize();

        // Write default configuration file ONLY if it did not exist initially
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(serialized) = toml::to_string_pretty(&default_cfg) {
            let _ = std::fs::write(&path, serialized);
        }

        default_cfg
    }

    /// Performs defensive boundary clamping to protect against invalid or absurd values.
    pub fn sanitize(&mut self) {
        // Clamping debounce_ms: [MIN_DEBOUNCE_MS, MAX_DEBOUNCE_MS]
        if self.watcher.debounce_ms < MIN_DEBOUNCE_MS || self.watcher.debounce_ms > MAX_DEBOUNCE_MS
        {
            let clamped = self
                .watcher
                .debounce_ms
                .clamp(MIN_DEBOUNCE_MS, MAX_DEBOUNCE_MS);
            log::warn!(
                target: log_targets::CORE,
                "Sanitizing watcher.debounce_ms: {} -> {} ms",
                self.watcher.debounce_ms,
                clamped
            );
            self.watcher.debounce_ms = clamped;
        }

        // Clamping default_memory_limit_mb: [MIN_MEMORY_LIMIT_MB, MAX_MEMORY_LIMIT_MB]
        if self.runtime.default_memory_limit_mb < MIN_MEMORY_LIMIT_MB
            || self.runtime.default_memory_limit_mb > MAX_MEMORY_LIMIT_MB
        {
            let clamped = self
                .runtime
                .default_memory_limit_mb
                .clamp(MIN_MEMORY_LIMIT_MB, MAX_MEMORY_LIMIT_MB);
            log::warn!(
                target: log_targets::CORE,
                "Sanitizing runtime.default_memory_limit_mb: {} -> {} MB",
                self.runtime.default_memory_limit_mb,
                clamped
            );
            self.runtime.default_memory_limit_mb = clamped;
        }

        // Clamping max_table_elements: [MIN_TABLE_ELEMENTS, MAX_TABLE_ELEMENTS]
        if self.runtime.max_table_elements < MIN_TABLE_ELEMENTS
            || self.runtime.max_table_elements > MAX_TABLE_ELEMENTS
        {
            let clamped = self
                .runtime
                .max_table_elements
                .clamp(MIN_TABLE_ELEMENTS, MAX_TABLE_ELEMENTS);
            log::warn!(
                target: log_targets::CORE,
                "Sanitizing runtime.max_table_elements: {} -> {}",
                self.runtime.max_table_elements,
                clamped
            );
            self.runtime.max_table_elements = clamped;
        }

        // Clamping frame watchdog: [MIN_FRAME_EPOCH_DEADLINE, MAX_FRAME_EPOCH_DEADLINE]
        self.runtime.frame_epoch_deadline = self
            .runtime
            .frame_epoch_deadline
            .clamp(MIN_FRAME_EPOCH_DEADLINE, MAX_FRAME_EPOCH_DEADLINE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitization_clamps_absurd_values() {
        let mut cfg = HostConfig {
            core: CoreConfig::default_for(BackendType::Metamod),
            logging: LogConfig::default(),
            watcher: WatcherConfig {
                enabled: true,
                debounce_ms: 9999999,
                on_file_deleted: OnFileDeleted::Keep,
                watch_configs: true,
            },
            runtime: RuntimeConfig {
                default_memory_limit_mb: 2, // too low
                max_table_elements: 0,      // too low
                frame_epoch_deadline: 0,
                event_epoch_deadline: 10,
                command_epoch_deadline: 10,
                load_epoch_deadline: 50,
            },
            pipeline: crate::config::ConsolePipelineConfig::default(),
        };

        cfg.sanitize();

        assert_eq!(cfg.watcher.debounce_ms, 5000);
        assert_eq!(cfg.runtime.default_memory_limit_mb, 16);
        assert_eq!(cfg.runtime.max_table_elements, 100);
        assert_eq!(cfg.runtime.frame_epoch_deadline, 1);
    }
}
