//! Host-side runtime orchestrator, engine bridge, configuration, storage, and hook dispatcher for GoldSrc.rs.

pub mod api_registry;
pub mod backend;
pub mod bundle;
pub mod chat;
#[cfg(feature = "cli")]
pub mod cli;
pub mod config;
pub mod extension;
pub mod features;
pub mod hardware;
pub mod hooks;
pub mod host;
pub mod hud;
pub mod i18n;
pub mod logging;
pub mod menu;
pub mod moderation;
pub mod net;
pub mod paths;
pub mod placeholders;
pub mod plugins;
pub mod preset;
pub mod rules;
pub mod session;
pub mod storage;
pub mod timer;
pub mod watcher;

pub use ::log;
pub use bundle::{BrokerError, BundleFsSandbox, BundleMessageBroker, SandboxError};
pub use chat::{
    ChatTargetResolver, process_chat_message, register_chat_target_resolver,
    unregister_chat_target_resolver,
};
pub use config::plugins as plugins_config;
pub use config::{
    ConfigBinder, HostConfig, PluginDebugConfig, PluginDebugSetting, PluginEntry, PluginGroup,
    PluginsConfig, SelfHealingConfigEngine,
};
pub use extension::{ExtensionRegistry, extension_registry};
pub use hardware::{SystemInfoService, SystemMetricsSnapshot, system_info};
pub use hooks::{VTableBridge, VTableTier};
pub use host::{EventPayload, HostEvent, HostRuntime, PlayerEvent};
pub use i18n::I18nService;
pub use net::NetworkMessageDispatcher;
pub use paths::PathResolver;
pub use placeholders::{PlaceholderRegistry, format_placeholders};
pub use plugins::PluginOrchestrator;
pub use preset::{ExecOutcome, PresetEngine, PresetError, PresetManifest, PresetStateSnapshot};
pub use storage::{Bucket, JsonFormat, SqliteStorageEngine, StorageFormat};
pub use timer::TimerService;
pub use watcher::{
    WatchTarget, WatcherEvent, WatcherFilter, WatcherService, WatcherSpec, WatcherStatus,
};
