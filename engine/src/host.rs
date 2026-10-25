use crate::{HostConfig, paths::PathResolver};
use goldsrc_api::consts::{BackendType, log_targets};
use goldsrc_host_wasm::PluginManager;
use goldsrc_host_wasm::error::HostError;
use goldsrc_spi::storage::StoragePort;

pub struct HostRuntime {
    pub backend: BackendType,
    manager: PluginManager,
    engine: std::sync::Arc<dyn goldsrc_spi::engine::EnginePort>,
    pub storage: std::sync::Arc<crate::storage::SqliteStorageEngine>,
    pub plugins_config: crate::plugins_config::PluginsConfig,
    pub paused_plugins: std::collections::HashMap<String, bool>,
    pub current_map: String,
    pub rule_orchestrator: crate::rules::RuleOrchestrator,
    pub watcher_service: crate::watcher::WatcherService,
    pub sessions: crate::session::ClientSessionManager,
    pub timer_service: std::sync::Arc<crate::timer::TimerService>,
}

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static RUNTIME: OnceLock<Mutex<HostRuntime>> = OnceLock::new();
static ENGINE_INSTANCE: OnceLock<std::sync::Arc<dyn goldsrc_spi::engine::EnginePort>> =
    OnceLock::new();
static BACKEND_TYPE: OnceLock<BackendType> = OnceLock::new();

thread_local! {
    static IN_MANAGER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Player-specific gameplay and lifecycle events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerEvent {
    Connect,
    Disconnect,
    PutInServer,
    UserInfoChanged,
    PreThink,
    PostThink,
    CmdEnd,
    Kill,
    SpectatorConnect,
    SpectatorDisconnect,
    SpectatorThink,
}

impl PlayerEvent {
    /// Returns the canonical event name string for WASM plugins.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Connect => "client_connect",
            Self::Disconnect => "client_disconnect",
            Self::PutInServer => "client_put_in_server",
            Self::UserInfoChanged => "client_user_info_changed",
            Self::PreThink => "player_pre_think",
            Self::PostThink => "player_post_think",
            Self::CmdEnd => "cmd_end",
            Self::Kill => "client_kill",
            Self::SpectatorConnect => "spectator_connect",
            Self::SpectatorDisconnect => "spectator_disconnect",
            Self::SpectatorThink => "spectator_think",
        }
    }
}

/// Inline fixed-capacity buffer for event payloads to eliminate heap allocations.
#[derive(Debug, Clone, Copy)]
pub enum EventPayload<'a> {
    Empty,
    Borrowed(&'a [u8]),
    Inline4([u8; 4]),
    Inline8([u8; 8]),
    Inline12([u8; 12]),
    Inline20([u8; 20]),
}

impl<'a> std::ops::Deref for EventPayload<'a> {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Empty => &[],
            Self::Borrowed(s) => s,
            Self::Inline4(s) => s,
            Self::Inline8(s) => s,
            Self::Inline12(s) => s,
            Self::Inline20(s) => s,
        }
    }
}

/// Lifecycle and gameplay events originating from the game engine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HostEvent<'a> {
    /// Server frame tick (occurs every engine frame).
    ServerFrame,
    /// Server map activation (ServerActivate).
    ServerActivate,
    /// Server map deactivation / map end / shutdown (ServerDeactivate).
    ServerDeactivate,
    /// Map changed explicitly.
    MapChange { map_name: &'a str },

    /// Player-specific event with slot index (1..=32).
    Player { slot: i32, event: PlayerEvent },

    /// User command execution start with buttons bitmask.
    CmdStart { slot: i32, buttons: u16 },

    /// Entity touch event.
    EntityTouch { touched: i32, other: i32 },

    /// Entity use event.
    EntityUse { used: i32, other: i32 },

    /// Menu item selection event by player.
    MenuSelect { player: i32, item_id: u32 },

    /// Entity take damage hook event (pre or post).
    EntityTakeDamage {
        victim: i32,
        inflictor: i32,
        attacker: i32,
        damage: f32,
        bits_damage_type: i32,
        timing: crate::hooks::types::HookTiming,
    },

    /// Entity killed hook event (pre or post).
    EntityKilled {
        victim: i32,
        attacker: i32,
        gib_mode: i32,
        timing: crate::hooks::types::HookTiming,
    },

    /// Custom or generic event with explicit name and byte payload.
    Custom { name: &'a str, payload: &'a [u8] },
}

impl<'a> HostEvent<'a> {
    /// Returns the canonical event name string for WASM plugins.
    pub fn name(&self) -> &str {
        match self {
            Self::ServerFrame => "server_frame",
            Self::ServerActivate => "server_activate",
            Self::ServerDeactivate => "server_deactivate",
            Self::MapChange { .. } => "map_change",
            Self::Player { event, .. } => event.as_str(),
            Self::CmdStart { .. } => "cmd_start",
            Self::EntityTouch { .. } => "entity_touch",
            Self::EntityUse { .. } => "entity_use",
            Self::MenuSelect { .. } => "menu_select",
            Self::EntityTakeDamage { timing, .. } => match timing {
                crate::hooks::types::HookTiming::Pre => "entity_take_damage_pre",
                crate::hooks::types::HookTiming::Post => "entity_take_damage_post",
            },
            Self::EntityKilled { timing, .. } => match timing {
                crate::hooks::types::HookTiming::Pre => "entity_killed_pre",
                crate::hooks::types::HookTiming::Post => "entity_killed_post",
            },
            Self::Custom { name, .. } => name,
        }
    }

    /// Encodes the binary payload for WASM plugins on the stack with zero heap allocation.
    pub fn payload(&self) -> EventPayload<'a> {
        match self {
            Self::ServerFrame | Self::ServerActivate | Self::ServerDeactivate => {
                EventPayload::Empty
            }
            Self::MapChange { map_name } => EventPayload::Borrowed(map_name.as_bytes()),
            Self::Player { slot, .. } => EventPayload::Inline4(slot.to_le_bytes()),
            Self::CmdStart { slot, buttons } => {
                let mut buf = [0u8; 8];
                buf[0..4].copy_from_slice(&slot.to_le_bytes());
                buf[4..6].copy_from_slice(&buttons.to_le_bytes());
                EventPayload::Inline8(buf)
            }
            Self::EntityTouch { touched, other }
            | Self::EntityUse {
                used: touched,
                other,
            } => EventPayload::Inline8(crate::api_registry::pack_two_i32(*touched, *other)),
            Self::MenuSelect { player, item_id } => {
                let mut buf = [0u8; 8];
                buf[0..4].copy_from_slice(&player.to_le_bytes());
                buf[4..8].copy_from_slice(&item_id.to_le_bytes());
                EventPayload::Inline8(buf)
            }
            Self::EntityTakeDamage {
                victim,
                inflictor,
                attacker,
                damage,
                bits_damage_type,
                ..
            } => {
                let mut buf = [0u8; 20];
                buf[0..4].copy_from_slice(&victim.to_le_bytes());
                buf[4..8].copy_from_slice(&inflictor.to_le_bytes());
                buf[8..12].copy_from_slice(&attacker.to_le_bytes());
                buf[12..16].copy_from_slice(&damage.to_le_bytes());
                buf[16..20].copy_from_slice(&bits_damage_type.to_le_bytes());
                EventPayload::Inline20(buf)
            }
            Self::EntityKilled {
                victim,
                attacker,
                gib_mode,
                ..
            } => {
                let mut buf = [0u8; 12];
                buf[0..4].copy_from_slice(&victim.to_le_bytes());
                buf[4..8].copy_from_slice(&attacker.to_le_bytes());
                buf[8..12].copy_from_slice(&gib_mode.to_le_bytes());
                EventPayload::Inline12(buf)
            }
            Self::Custom { payload, .. } => EventPayload::Borrowed(payload),
        }
    }
}

impl HostRuntime {
    /// Initialize the host runtime, logger, configuration, storage, i18n and hot reload watchers.
    ///
    /// `engine` is the backend's [`goldsrc_spi::engine::EnginePort`] bridge — it gives
    /// WASM plugins access to the real game state. Call once at backend init.
    pub fn init(
        backend: BackendType,
        print_cb: fn(&str),
        engine: std::sync::Arc<dyn goldsrc_spi::engine::EnginePort>,
    ) -> Result<(), HostError> {
        let backend_name = match backend {
            BackendType::Metamod => "Metamod",
            BackendType::Standalone => "Standalone",
        };
        goldsrc_host_wasm::set_print_callback(print_cb);
        goldsrc_host_wasm::set_show_menu_callback(|_player_idx, _keys_mask, _timeout, _text| {});
        goldsrc_host_wasm::set_time_callback(HostRuntime::current_time);

        goldsrc_host_wasm::set_storage_callbacks(
            |bucket, key| HostRuntime::storage().and_then(|s| s.get(bucket, key).ok().flatten()),
            |bucket, key, val| {
                HostRuntime::storage()
                    .map(|s| s.set(bucket, key, val).is_ok())
                    .unwrap_or(false)
            },
            |bucket, key| {
                HostRuntime::storage()
                    .map(|s| s.delete(bucket, key).unwrap_or(false))
                    .unwrap_or(false)
            },
            |bucket, key, delta| {
                HostRuntime::storage()
                    .and_then(|s| s.fetch_add(bucket, key, delta).ok())
                    .unwrap_or(0)
            },
        );

        let _ = ENGINE_INSTANCE.set(engine.clone());
        let _ = BACKEND_TYPE.set(backend);

        goldsrc_host_wasm::set_translate_callback(|caller, dict, lang, key| {
            crate::i18n::I18nService::translate_with_caller(caller, dict, lang, key, &[], &[])
        });

        goldsrc_host_wasm::set_vfs_callbacks(
            |_caller, rel_path| {
                if rel_path.contains("..") {
                    return Err("Access denied: path traversal not permitted".to_string());
                }
                let resolved = Self::resolve_vfs_path(rel_path)
                    .ok_or_else(|| format!("File not found: {rel_path}"))?;
                std::fs::read_to_string(&resolved)
                    .map_err(|e| format!("Failed to read '{rel_path}': {e}"))
            },
            |_caller, rel_path| {
                if rel_path.contains("..") {
                    return Err("Access denied: path traversal not permitted".to_string());
                }
                let resolved = Self::resolve_vfs_path(rel_path)
                    .ok_or_else(|| format!("File not found: {rel_path}"))?;
                std::fs::read(&resolved).map_err(|e| format!("Failed to read '{rel_path}': {e}"))
            },
            |_caller, rel_path| {
                if rel_path.contains("..") {
                    return Err("Access denied: path traversal not permitted".to_string());
                }
                let backend = BACKEND_TYPE.get().copied().unwrap_or(BackendType::Metamod);
                let fw_dir = PathResolver::framework_dir(backend);
                let resolved = if rel_path == "." || rel_path.is_empty() {
                    fw_dir
                } else if let Some(found) = Self::resolve_vfs_path(rel_path) {
                    found
                } else {
                    return Err(format!("Directory not found: {rel_path}"));
                };
                let read_dir = std::fs::read_dir(&resolved)
                    .map_err(|e| format!("Failed to read directory '{rel_path}': {e}"))?;
                let mut entries = Vec::new();
                for entry in read_dir.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let metadata = entry.metadata().ok();
                    let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                    let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                    entries.push((name, is_dir, size));
                }
                Ok(entries)
            },
        );

        goldsrc_host_wasm::set_format_placeholders_callback(|player_idx, text| {
            let player = if goldsrc_api::PlayerSlot::VALID_RANGE.contains(&player_idx) {
                Some(goldsrc_api::Player::new(player_idx))
            } else {
                None
            };
            crate::placeholders::format_placeholders(text, player)
        });

        goldsrc_host_wasm::set_feature_query_callback(|token| {
            crate::features::has_feature(goldsrc_spi::hash::FeatureToken(token))
        });
        goldsrc_api::engine::server::set_feature_query_hook(|token| {
            crate::features::has_feature(goldsrc_spi::hash::FeatureToken(token))
        });

        goldsrc_api::client::player::set_player_resolver_hook(|index| {
            if let Some(engine) = HostRuntime::engine() {
                if engine.player_is_valid(index) {
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        Some(goldsrc_api::Player::from_index(index))
                    }
                    #[cfg(target_arch = "wasm32")]
                    {
                        Some(goldsrc_api::Player::new(index))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        });
        goldsrc_api::client::player::set_player_validity_hook(|index| {
            if let Some(engine) = HostRuntime::engine() {
                engine.player_is_valid(index)
            } else {
                false
            }
        });
        goldsrc_api::client::player::set_player_name_hook(|index| {
            if let Some(engine) = HostRuntime::engine() {
                engine.player_name(index)
            } else {
                None
            }
        });
        goldsrc_api::client::player::set_player_team_hook(|index| {
            if let Some(engine) = HostRuntime::engine() {
                engine.player_team(index)
            } else {
                0
            }
        });
        goldsrc_api::client::player::set_player_lang_hook(|index| {
            if let Some(override_lang) = HostRuntime::with_sessions(|s| {
                s.get(index)
                    .and_then(|sess| sess.lang().map(str::to_string))
            })
            .flatten()
            {
                Some(override_lang)
            } else if let Some(engine) = HostRuntime::engine() {
                engine.player_lang(index)
            } else {
                None
            }
        });
        goldsrc_api::client::player::set_player_identity_hook(|index| {
            if let Some(engine) = HostRuntime::engine() {
                engine.player_identity(index)
            } else {
                goldsrc_api::client::PlayerIdentity {
                    slot: index,
                    user_id: 0,
                    raw_auth_id: "STEAM_ID_PENDING".to_string(),
                    auth_state: goldsrc_api::client::AuthState::Pending,
                    ip: None,
                    ping: 0,
                    packet_loss: 0,
                    is_bot: false,
                    is_hltv: false,
                }
            }
        });
        goldsrc_api::client::player::set_player_session_token_hook(|index| {
            HostRuntime::with_sessions(|s| s.get(index).map(|sess| sess.token())).flatten()
        });
        goldsrc_api::client::player::set_native_print_hook(|player_index, target, message| {
            if let Some(engine) = HostRuntime::engine() {
                crate::net::NetworkMessageDispatcher::dispatch_player_print(
                    engine.as_ref(),
                    player_index,
                    target,
                    message,
                );
            }
        });
        goldsrc_api::client::player::set_open_menu_hook(|player_idx, menu| {
            if let Some(engine) = HostRuntime::engine() {
                let current_time = HostRuntime::current_time();
                if let Ok(mut mgr) = crate::menu::menu_manager().lock() {
                    mgr.open_menu(player_idx, menu.clone(), engine.as_ref(), current_time);
                }
            }
        });
        crate::menu::init_menu_hooks();

        let mut manager = PluginManager::new(engine.clone())
            .map_err(|e| HostError::Manager(format!("[GoldSrc.rs {backend_name}] {e}")))?;
        manager.set_plugin_dirs(crate::paths::PathResolver::plugin_dirs(backend));

        let sys_config = HostConfig::load_or_create(backend);

        // Initialise unified logger
        let logs_dir = crate::paths::PathResolver::existing_log_dir(backend);
        crate::logging::init_with_dir(
            sys_config.logging.clone(),
            Some(logs_dir.clone()),
            backend,
            Some(print_cb),
        );

        // Initialize structured reports service
        crate::reports::init_reports(logs_dir);

        // Initial startup banner stating active backend and version
        log::info!(
            target: log_targets::CORE,
            "GoldSrc.rs v{} initialized (Backend: {})",
            env!("CARGO_PKG_VERSION"),
            backend_name
        );

        let main_cfg_path = PathResolver::main_config_path(backend);
        log::info!(
            target: log_targets::CORE,
            "Config loaded from: \"{}\"",
            PathResolver::normalize(&main_cfg_path)
        );

        // 1. Initialize SQLite WAL Storage Engine in data/db/goldsrc.db
        let db_path = crate::paths::PathResolver::db_path(backend);
        let storage = match crate::storage::SqliteStorageEngine::open(&db_path) {
            Ok(s) => {
                log::info!(
                    target: log_targets::STORAGE,
                    "SQLite WAL Storage Engine initialized at \"{}\"",
                    PathResolver::normalize(&db_path)
                );
                s
            }
            Err(e) => {
                log::error!(
                    target: log_targets::STORAGE,
                    "Failed to initialize SQLite Storage Engine at \"{}\": {e}",
                    PathResolver::normalize(&db_path)
                );
                return Err(HostError::Manager(format!("Storage init failed: {e}")));
            }
        };

        // 2. Initialize i18n dictionaries from data/lang/*.toml
        let lang_dir = crate::paths::PathResolver::lang_dir(backend);
        if !lang_dir.exists() {
            let _ = std::fs::create_dir_all(&lang_dir);
        }
        let common_lang_file = lang_dir.join("common.toml");
        if !common_lang_file.exists() {
            let default_common = include_str!("resources/common.toml");
            let _ = std::fs::write(&common_lang_file, default_common);
        }
        let sample_lang_file = lang_dir.join("test_i18n.toml");
        if !sample_lang_file.exists() {
            let default_template = include_str!("resources/test_i18n.toml");
            let _ = std::fs::write(&sample_lang_file, default_template);
        }
        let lang_count = crate::i18n::I18nService::load_dir(&lang_dir);
        log::info!(
            target: log_targets::I18N,
            "Loaded {lang_count} localization entries from \"{}\"",
            PathResolver::normalize(&lang_dir)
        );

        // 3. Initialize watcher service and register default watchers
        let mut watcher_service = crate::watcher::WatcherService::new();
        let mut registered_watchers = Vec::new();

        let existing_plugin_dir = crate::paths::PathResolver::existing_plugin_dir(backend);
        if let Ok(()) = watcher_service.register(crate::watcher::WatcherSpec::directory(
            "core:plugins",
            &existing_plugin_dir,
            crate::watcher::WatcherFilter::Extension("wasm"),
            true,
        )) {
            registered_watchers.push("core:plugins");
        }

        let config_dir = crate::paths::PathResolver::existing_config_dir(backend);
        let plugins_config_path = config_dir.join("plugins.toml");
        if let Ok(()) = watcher_service.register(crate::watcher::WatcherSpec::file(
            "core:configs",
            &plugins_config_path,
        )) {
            registered_watchers.push("core:configs");
        }

        if let Ok(()) = watcher_service.register(crate::watcher::WatcherSpec::directory(
            "i18n:dicts",
            &lang_dir,
            crate::watcher::WatcherFilter::Extension("toml"),
            true,
        )) {
            registered_watchers.push("i18n:dicts");
        }

        if !registered_watchers.is_empty() {
            log::info!(
                target: log_targets::CORE,
                "Hot-reload watchers active ({}/3): [{}]",
                registered_watchers.len(),
                registered_watchers.join(", ")
            );
        }

        // Load or create plugins.toml configuration template
        let plugins_config_path = config_dir.join("plugins.toml");
        let plugins_config =
            crate::plugins_config::PluginsConfig::load_or_create(&plugins_config_path);
        log::info!(
            target: log_targets::WASM,
            "Plugins orchestration config loaded from: \"{}\"",
            crate::paths::PathResolver::normalize(&plugins_config_path)
        );

        // Recursive helper to discover all .wasm plugins in directory tree
        fn discover_wasm_plugins(
            dir: &std::path::Path,
            base_dir: &std::path::Path,
            out: &mut Vec<(String, std::path::PathBuf)>,
        ) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        discover_wasm_plugins(&path, base_dir, out);
                    } else if path.extension().is_some_and(|ext| ext == "wasm")
                        && let Ok(rel) = path.strip_prefix(base_dir)
                    {
                        let rel_str = rel.with_extension("").to_string_lossy().replace('\\', "/");
                        out.push((rel_str, path));
                    }
                }
            }
        }

        let mut discovered_plugins = Vec::new();
        let plugin_dir = crate::paths::PathResolver::existing_plugin_dir(backend);
        discover_wasm_plugins(&plugin_dir, &plugin_dir, &mut discovered_plugins);

        // Resolve plugin load order deterministically using PhasedDag:
        // Tier (Core -> Service -> Gameplay -> Addon -> Analytics) -> Topological Dependencies (`requires`) -> Discovery Order
        let mut dag = goldsrc_api::dag::PhasedDag::<
            goldsrc_api::dag::PluginTier,
            String,
            std::path::PathBuf,
        >::new();
        for (rel_name, path) in &discovered_plugins {
            let base_name = rel_name
                .rsplit_once('/')
                .map(|(_, b)| b)
                .unwrap_or(rel_name);
            let entry = plugins_config
                .plugins
                .iter()
                .find(|p| p.name == *rel_name || p.name == base_name);
            let tier = entry.map(|p| p.tier).unwrap_or_default();
            let mut builder = dag.add(rel_name.clone(), path.clone()).phase(tier);
            if let Some(e) = entry {
                for req in &e.requires {
                    let parsed_req = req.parse::<goldsrc_api::Requirement>();
                    let dep_name: &str = match &parsed_req {
                        Ok(goldsrc_api::Requirement::Plugin { name, .. }) => name.as_str(),
                        // Non-plugin requirements (ext:*, cvar:*, feature:*, etc.) are environmental, not inter-plugin edges
                        Ok(_) => continue,
                        Err(_) => req.as_str(),
                    };
                    let target_rel = discovered_plugins
                        .iter()
                        .find(|(d_name, _)| {
                            d_name == dep_name
                                || d_name.rsplit_once('/').map(|(_, b)| b).unwrap_or(d_name)
                                    == dep_name
                        })
                        .map(|(d_name, _)| d_name.clone())
                        .unwrap_or_else(|| dep_name.to_string());
                    builder = builder.after(target_rel);
                }
            }
            builder.register();
        }

        let sorted_plugins: Vec<(String, std::path::PathBuf)> = match dag.resolve() {
            Ok(resolved) => resolved.into_iter().map(|n| (n.id, n.data)).collect(),
            Err(e) => {
                log::error!(
                    target: log_targets::WASM,
                    "Plugin topological resolution encountered conflict: {e}. Falling back to default discovery order."
                );
                discovered_plugins
            }
        };

        // Load plugins based on plugins.toml activation status with consolidated batch report
        let mut active_plugins = Vec::new();
        let mut paused_plugins_list = Vec::new();
        let mut failed_plugins = Vec::new();

        for (rel_name, path) in sorted_plugins {
            let is_enabled = plugins_config.is_plugin_enabled(&rel_name);
            match manager.load_plugin(&path) {
                Ok(plugin_name) => {
                    if !is_enabled {
                        let _ = manager.pause_plugin(&plugin_name, true);
                        paused_plugins_list.push(rel_name);
                    } else {
                        active_plugins.push(rel_name);
                    }
                }
                Err(e) => {
                    failed_plugins.push((rel_name, e.to_string()));
                }
            }
        }

        let total = active_plugins.len() + paused_plugins_list.len() + failed_plugins.len();
        log::info!(
            target: log_targets::WASM,
            "Plugins startup summary: {total} total ({active} active, {paused} paused, {failed} failed)",
            active = active_plugins.len(),
            paused = paused_plugins_list.len(),
            failed = failed_plugins.len(),
        );
        if !active_plugins.is_empty() {
            log::info!(
                target: log_targets::WASM,
                "  Active plugins: [{}]",
                active_plugins.join(", ")
            );
        }
        if !paused_plugins_list.is_empty() {
            log::info!(
                target: log_targets::WASM,
                "  Paused plugins: [{}]",
                paused_plugins_list.join(", ")
            );
        }
        for (name, err) in failed_plugins {
            log::error!(
                target: log_targets::WASM,
                "  Failed to load '{name}': {err}"
            );
        }

        let paused_plugins = std::collections::HashMap::new();
        let mut rule_orchestrator = crate::rules::RuleOrchestrator::new();
        let initial_rules = plugins_config.rules.iter().map(|r| r.to_rule()).collect();
        rule_orchestrator.set_rules(initial_rules);

        let runtime = Self {
            backend,
            manager,
            engine,
            storage,
            plugins_config,
            paused_plugins,
            current_map: String::new(),
            rule_orchestrator,
            watcher_service,
            sessions: crate::session::ClientSessionManager::new(),
            timer_service: crate::timer::timer_service(),
        };
        let _ = RUNTIME.set(Mutex::new(runtime));

        // Evaluate initial rules (e.g. initial pause/cvar states) across all scopes
        Self::evaluate_rules("", 0);

        // Register default moderation command executors (kick, mute, ban)
        crate::moderation::set_engine_provider(HostRuntime::engine);
        crate::moderation::register_moderation_commands();

        // Register default chat command trigger adapter
        crate::chat::register_chat_trigger(std::sync::Arc::new(crate::chat::CommandChatTrigger));

        Ok(())
    }

    /// Resolves candidate relative path checking addon framework directories, mod dir, and server root.
    pub fn resolve_vfs_path(rel_path: &str) -> Option<std::path::PathBuf> {
        let p = std::path::Path::new(rel_path);

        let backend = BACKEND_TYPE.get().copied().unwrap_or(BackendType::Metamod);
        let fw_dir = PathResolver::framework_dir(backend);

        // Candidate 1: inside framework dir (e.g. cstrike/addons/goldsrc/<rel_path> or <mod>/goldsrc/<rel_path>)
        let in_fw = fw_dir.join(p);
        if in_fw.exists() {
            return Some(in_fw);
        }

        // Candidate 2: inside configs dir
        let in_configs = PathResolver::existing_config_dir(backend).join(p);
        if in_configs.exists() {
            return Some(in_configs);
        }

        // Candidate 3: inside logs dir
        let in_logs = PathResolver::existing_log_dir(backend).join(p);
        if in_logs.exists() {
            return Some(in_logs);
        }

        // Candidate 4: inside plugins dir
        let in_plugins = PathResolver::existing_plugin_dir(backend).join(p);
        if in_plugins.exists() {
            return Some(in_plugins);
        }

        // Candidate 5: fallback without mod prefix (addons/goldsrc/<rel_path>)
        let in_alt = std::path::PathBuf::from(goldsrc_api::consts::ADDONS_DIR_NAME)
            .join(goldsrc_api::consts::FRAMEWORK_NAME)
            .join(p);
        if in_alt.exists() {
            return Some(in_alt);
        }

        None
    }

    /// Returns a clone of the Engine reference if initialized.
    pub fn engine() -> Option<std::sync::Arc<dyn goldsrc_spi::engine::EnginePort>> {
        ENGINE_INSTANCE.get().cloned()
    }

    /// Returns the global host `TimerService` instance.
    pub fn timer_service() -> std::sync::Arc<crate::timer::TimerService> {
        crate::timer::timer_service()
    }

    /// Returns monotonic server host uptime in seconds (f32).
    pub fn current_time() -> f32 {
        static START_TIME: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START_TIME
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f32()
    }

    /// Returns the currently active map name.
    pub fn current_map() -> String {
        RUNTIME
            .get()
            .and_then(|lock| lock.lock().ok().map(|g| g.current_map.clone()))
            .unwrap_or_default()
    }

    /// Sets the active map name.
    pub fn set_current_map(map_name: &str) {
        if let Some(lock) = RUNTIME.get() {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            guard.current_map = map_name.to_string();
        }
    }

    /// Returns a reference to the shared SQLite Storage Engine.
    pub fn storage() -> Option<std::sync::Arc<crate::storage::SqliteStorageEngine>> {
        RUNTIME
            .get()
            .and_then(|lock| lock.lock().ok().map(|g| g.storage.clone()))
    }

    /// Sets a deliberate administrator manual pause state override for a plugin.
    pub fn set_manual_pause_override(plugin_name: &str, is_paused: bool) {
        if let Some(lock) = RUNTIME.get() {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .rule_orchestrator
                .set_manual_override(plugin_name, is_paused);
        }
    }

    /// Removes a deliberate administrator manual pause state override for a plugin.
    pub fn remove_manual_pause_override(plugin_name: &str) -> Option<bool> {
        if let Some(lock) = RUNTIME.get() {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            guard.rule_orchestrator.remove_manual_override(plugin_name)
        } else {
            None
        }
    }

    /// Clears all deliberate administrator manual pause state overrides.
    pub fn clear_manual_pause_overrides() {
        if let Some(lock) = RUNTIME.get() {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            guard.rule_orchestrator.clear_manual_overrides();
        }
    }

    fn handle_map_change() {
        if let Some(lock) = RUNTIME.get() {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            let _ = guard.storage.flush();
            guard.paused_plugins.clear();
            let map_name = guard.current_map.clone();
            guard.current_map.clear();
            guard.rule_orchestrator.on_map_change();
            guard.sessions.clear();
            crate::extension::extension_registry().on_change_level(&map_name);
        }
        crate::logging::flush();
    }

    /// Returns `true` if the current thread is already executing inside `HostRuntime::with_manager`.
    #[inline(always)]
    pub fn is_in_manager() -> bool {
        IN_MANAGER.get()
    }

    /// Run `f` with exclusive access to the `PluginManager`, if initialized.
    /// Protects against re-entrant mutex deadlock if called recursively on the same thread.
    pub fn with_manager<R>(f: impl FnOnce(Option<&mut PluginManager>) -> R) -> R {
        if IN_MANAGER.get() {
            log::trace!(
                target: log_targets::CORE,
                "Re-entrant call to HostRuntime::with_manager suppressed to prevent deadlock"
            );
            return f(None);
        }

        if let Some(lock) = RUNTIME.get() {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            IN_MANAGER.set(true);
            struct ResetGuard;
            impl Drop for ResetGuard {
                fn drop(&mut self) {
                    IN_MANAGER.set(false);
                }
            }
            let _reset = ResetGuard;
            f(Some(&mut guard.manager))
        } else {
            f(None)
        }
    }

    /// Returns the active backend type (Metamod or Standalone).
    pub fn backend_type() -> BackendType {
        BACKEND_TYPE.get().copied().unwrap_or(BackendType::Metamod)
    }

    /// Run `f` with exclusive access to the host runtime's [`WatcherService`], if initialized.
    pub fn with_watcher_service<R>(
        f: impl FnOnce(Option<&mut crate::watcher::WatcherService>) -> R,
    ) -> R {
        if let Some(lock) = RUNTIME.get() {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            f(Some(&mut guard.watcher_service))
        } else {
            f(None)
        }
    }

    /// Runs a closure with read-only access to [`ClientSessionManager`].
    pub fn with_sessions<R>(
        f: impl FnOnce(&crate::session::ClientSessionManager) -> R,
    ) -> Option<R> {
        RUNTIME.get().and_then(|lock| {
            let guard = lock.lock().ok()?;
            Some(f(&guard.sessions))
        })
    }

    /// Runs a closure with mutable access to [`ClientSessionManager`].
    pub fn with_sessions_mut<R>(
        f: impl FnOnce(&mut crate::session::ClientSessionManager) -> R,
    ) -> Option<R> {
        RUNTIME.get().map(|lock| {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut guard.sessions)
        })
    }

    /// Dispatches and processes an engine lifecycle event across all host subsystems.
    pub fn on(event: HostEvent<'_>) {
        match event {
            HostEvent::ServerFrame => {
                Self::handle_server_frame();
            }
            HostEvent::ServerActivate => {
                let now = Self::current_time();
                if let Ok(mut mgr) = crate::menu::menu_manager().lock() {
                    mgr.on_round_start(1, now);
                }
                if let Some(engine) = Self::engine() {
                    let map_name = engine.cvar_get_string("mapname").unwrap_or_default();
                    crate::reports::start_map_session(&map_name);
                    let player_count = goldsrc_api::auth::Auth::total_players();
                    Self::evaluate_rules_scoped(
                        goldsrc_api::rules::RuleScope::MapChange,
                        &map_name,
                        player_count,
                    );
                }
            }
            HostEvent::ServerDeactivate => {
                crate::reports::finalize_map_session();
                goldsrc_api::bump_map_generation();
                goldsrc_api::auth::Auth::clear_all_players();
                goldsrc_host_wasm::clear_all_active_menu_owners();
                if let Ok(mut mgr) = crate::menu::menu_manager().lock() {
                    mgr.on_map_change();
                }
                Self::handle_map_change();
            }
            HostEvent::MapChange { map_name } => {
                crate::reports::finalize_map_session();
                if !map_name.is_empty() {
                    crate::reports::start_map_session(map_name);
                    Self::set_current_map(map_name);
                }
                Self::handle_map_change();
            }
            HostEvent::Player { slot, event } => match event {
                PlayerEvent::Connect => {
                    let player_count = goldsrc_api::auth::Auth::total_players();
                    let current_map = Self::current_map();
                    Self::evaluate_rules_scoped(
                        goldsrc_api::rules::RuleScope::PlayerCount,
                        &current_map,
                        player_count,
                    );

                    // Native ban enforcement check
                    if let Some(engine) = Self::engine() {
                        let auth = engine.player_auth_id(slot).unwrap_or_default();
                        let ip = engine.player_ip(slot).unwrap_or_default();
                        let ban = crate::moderation::is_identity_banned(&auth)
                            .or_else(|| crate::moderation::is_identity_banned(&ip));
                        if let Some(b) = ban {
                            crate::moderation::drop_client(
                                slot,
                                &format!("You are banned: {}", b.reason),
                            );
                        }
                    }
                }
                PlayerEvent::Disconnect => {
                    crate::reports::record_event(
                        goldsrc_service_reports::ReportEvent::Disconnect {
                            player: slot,
                            auth: String::new(),
                            duration_connected_secs: 0,
                            reason: "disconnect".to_string(),
                        },
                    );
                    // 1. Session cleanup
                    let _ = Self::with_sessions_mut(|s| {
                        s.on_disconnect(slot);
                    });
                    // 2. Active menu cleanup
                    goldsrc_host_wasm::clear_active_menu_owner(slot);
                    if let Ok(mut mgr) = crate::menu::menu_manager().lock() {
                        mgr.on_disconnect(slot);
                    }
                    // 3. Auth registration cleanup
                    goldsrc_api::auth::Auth::remove_player(slot);
                    // 4. Reactive rules recalculation
                    let player_count = goldsrc_api::auth::Auth::total_players();
                    let current_map = Self::current_map();
                    Self::evaluate_rules_scoped(
                        goldsrc_api::rules::RuleScope::PlayerCount,
                        &current_map,
                        player_count,
                    );
                }
                PlayerEvent::UserInfoChanged => {
                    if let Some(engine) = Self::engine() {
                        let current_time = Self::current_time();
                        if let Ok(mut mgr) = crate::menu::menu_manager().lock() {
                            mgr.refresh_player_menu(slot, engine.as_ref(), current_time);
                        }
                    }
                }
                PlayerEvent::PutInServer => {
                    log::trace!(target: log_targets::CORE, "Player {slot} put in server");
                }
                PlayerEvent::PreThink => {
                    log::trace!(target: log_targets::CORE, "Player {slot} pre-think");
                }
                PlayerEvent::PostThink => {
                    log::trace!(target: log_targets::CORE, "Player {slot} post-think");
                }
                PlayerEvent::CmdEnd => {
                    log::trace!(target: log_targets::CORE, "Player {slot} cmd end");
                }
                PlayerEvent::Kill => {
                    log::trace!(target: log_targets::CORE, "Player {slot} suicide / client kill");
                }
                PlayerEvent::SpectatorConnect => {
                    log::trace!(target: log_targets::CORE, "Spectator {slot} connect");
                }
                PlayerEvent::SpectatorDisconnect => {
                    log::trace!(target: log_targets::CORE, "Spectator {slot} disconnect");
                }
                PlayerEvent::SpectatorThink => {
                    log::trace!(target: log_targets::CORE, "Spectator {slot} think");
                }
            },
            HostEvent::EntityTakeDamage {
                victim,
                attacker,
                damage,
                ..
            } => {
                crate::reports::record_event(goldsrc_service_reports::ReportEvent::Damage {
                    attacker,
                    victim,
                    damage,
                    weapon: String::new(),
                    hitgroup: 0,
                    timestamp_ms: Self::current_time() as u64,
                });
            }
            HostEvent::EntityKilled {
                victim, attacker, ..
            } => {
                crate::reports::record_event(goldsrc_service_reports::ReportEvent::Kill {
                    attacker,
                    victim,
                    weapon: String::new(),
                    headshot: false,
                    timestamp_ms: Self::current_time() as u64,
                });
            }
            HostEvent::MenuSelect { player, item_id } => {
                if let Some(engine) = Self::engine() {
                    let current_time = Self::current_time();
                    if let Ok(mut mgr) = crate::menu::menu_manager().lock() {
                        let _ = mgr.handle_menuselect(
                            player,
                            item_id as u8,
                            engine.as_ref(),
                            current_time,
                        );
                    }
                }
            }
            HostEvent::CmdStart { slot, buttons } => {
                log::trace!(target: log_targets::CORE, "CmdStart slot={slot} buttons={buttons:#x}");
            }
            HostEvent::EntityTouch { touched, other } => {
                log::trace!(target: log_targets::CORE, "EntityTouch touched={touched} other={other}");
            }
            HostEvent::EntityUse { used, other } => {
                log::trace!(target: log_targets::CORE, "EntityUse used={used} other={other}");
            }
            HostEvent::Custom { name, payload } => {
                log::trace!(
                    target: log_targets::CORE,
                    "HostEvent::Custom name={name} len={}",
                    payload.len()
                );
            }
        }
    }

    /// Triggers reactive rule engine re-evaluation for current game state across all scopes.
    pub fn evaluate_rules(map_name: &str, player_count: usize) {
        Self::evaluate_rules_scoped(goldsrc_api::rules::RuleScope::All, map_name, player_count);
    }

    /// Triggers reactive rule engine re-evaluation for current game state under the specified [`RuleScope`].
    /// Drops the `HostRuntime` mutex during rule action execution to avoid deadlocks.
    pub fn evaluate_rules_scoped(
        scope: goldsrc_api::rules::RuleScope,
        map_name: &str,
        player_count: usize,
    ) {
        let Some(lock) = RUNTIME.get() else {
            return;
        };

        let backend = lock
            .lock()
            .map(|g| g.backend)
            .unwrap_or(BackendType::Metamod);

        // 1. Reload plugins.toml dynamically to pick up any changes made by the server administrator
        let config_dir = crate::paths::PathResolver::existing_config_dir(backend);
        let plugins_config_path = config_dir.join("plugins.toml");
        let fresh_config =
            crate::plugins_config::PluginsConfig::load_or_create(&plugins_config_path);

        // 2. Evaluate rules directly on the active RuleOrchestrator under lock
        let results = {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            guard.plugins_config = fresh_config;

            let rules: Vec<goldsrc_api::rules::Rule> = guard
                .plugins_config
                .rules
                .iter()
                .map(|r| r.to_rule())
                .collect();
            guard.rule_orchestrator.set_rules(rules);

            let resolved_map = if !map_name.is_empty() {
                map_name.to_string()
            } else if !guard.current_map.is_empty() {
                guard.current_map.clone()
            } else {
                guard.engine.cvar_get_string("mapname").unwrap_or_default()
            };

            if !resolved_map.is_empty() {
                guard.current_map = resolved_map.clone();
            }

            let effective_map = guard.current_map.clone();
            let manual_overrides = guard.rule_orchestrator.manual_overrides().clone();
            let engine = guard.engine.clone();

            let rt = &mut *guard;
            let results = {
                let mut ctx = crate::rules::ServerRuleContext {
                    map_name: &effective_map,
                    player_count,
                    engine: engine.as_ref(),
                    plugins_config: &mut rt.plugins_config,
                    paused_plugins: &mut rt.paused_plugins,
                    manual_overrides: &manual_overrides,
                    execution_log: Vec::new(),
                };
                rt.rule_orchestrator.evaluate_scope(&scope, &mut ctx)
            };

            // Synchronize plugin states with WASM manager
            crate::plugins::PluginOrchestrator::sync_plugin_states(
                &mut rt.manager,
                &rt.plugins_config,
                &rt.paused_plugins,
                &manual_overrides,
            );

            results
        };

        if !results.is_empty() {
            let executed: Vec<&str> = results
                .iter()
                .filter(|(_, res)| res.is_ok())
                .map(|(name, _)| name.as_str())
                .collect();
            let failed: Vec<(&str, &[String])> = results
                .iter()
                .filter_map(|(name, res)| match res {
                    Err(errs) => Some((name.as_str(), errs.as_slice())),
                    Ok(_) => None,
                })
                .collect();

            if !executed.is_empty() {
                log::info!(
                    target: log_targets::RULES,
                    "Reactive rules evaluated ({}/{} applied): [{}]",
                    executed.len(),
                    results.len(),
                    executed.join(", ")
                );
            }
            for (rule_name, errors) in failed {
                log::warn!(
                    target: log_targets::RULES,
                    "Failed to execute rule '{rule_name}': {errors:?}"
                );
            }
        }
    }

    fn handle_server_frame() {
        let events = if let Some(lock) = RUNTIME.get() {
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            guard.watcher_service.drain_events()
        } else {
            Vec::new()
        };

        for event in events {
            match event.watcher_id.as_str() {
                "core:plugins" => {
                    log::info!(
                        target: log_targets::WASM,
                        "Detected change in plugin file \"{}\", reloading...",
                        crate::paths::PathResolver::normalize(&event.path)
                    );
                    let path = &event.path;
                    if !path.exists() {
                        Self::with_manager(|m| {
                            if let Some(manager) = m {
                                manager.unload_plugin_by_path(path);
                            }
                        });
                        continue;
                    }

                    // 1. Snapshot engine pointers under brief lock
                    let engines = Self::with_manager(|m| {
                        m.map(|manager| (manager.wasm_engine(), manager.engine_ops()))
                    });

                    let Some((wasm_engine, engine_ops)) = engines else {
                        continue;
                    };

                    // 2. Heavy Cranelift compilation & component instantiation OUTSIDE lock (drop(guard))!
                    match goldsrc_host_wasm::manager::loader::instantiate_plugin(
                        &wasm_engine,
                        &engine_ops,
                        path,
                    ) {
                        Ok(new_plugin) => {
                            // 3. Short lock re-acquisition to commit reloaded plugin and swap instances
                            Self::with_manager(|m| {
                                if let Some(manager) = m {
                                    let _ = manager.commit_reloaded_plugin(new_plugin, path);
                                }
                            });
                        }
                        Err(e) => {
                            log::warn!(
                                target: log_targets::WASM,
                                "Hot-reload instantiation of \"{}\" failed (previous version kept active): {e}",
                                crate::paths::PathResolver::normalize(path)
                            );
                        }
                    }
                }
                "core:configs" => {
                    let path = &event.path;
                    let file_name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default();
                    if file_name.eq_ignore_ascii_case("plugins.toml") {
                        log::info!(
                            target: log_targets::WASM,
                            "Hot-reloaded plugins orchestration config from \"{}\"",
                            crate::paths::PathResolver::normalize(path)
                        );
                        let data = path.to_string_lossy().as_bytes().to_vec();
                        Self::with_manager(|m| {
                            if let Some(manager) = m {
                                manager.call_on_event("config_changed", &data);
                            }
                        });
                        Self::evaluate_rules("", 0);
                    }
                }
                "i18n:dicts" => {
                    let path = &event.path;
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                        && let Ok(count) = crate::i18n::I18nService::load_file(stem, path)
                    {
                        log::info!(
                            target: log_targets::I18N,
                            "Hot-reloaded {count} keys from \"{}\"",
                            crate::paths::PathResolver::normalize(path)
                        );
                        let data = path.to_string_lossy().as_bytes().to_vec();
                        Self::with_manager(|m| {
                            if let Some(manager) = m {
                                manager.call_on_event("config_changed", &data);
                            }
                        });
                        if let Some(engine) = Self::engine() {
                            let now = Self::current_time();
                            if let Ok(mut mgr) = crate::menu::menu_manager().lock() {
                                mgr.refresh_all_menus(engine.as_ref(), now);
                            }
                        }
                    }
                }
                _ => {
                    log::debug!(
                        target: log_targets::WATCHER,
                        "Unhandled watcher event '{}' for path \"{}\"",
                        event.watcher_id,
                        crate::paths::PathResolver::normalize(&event.path)
                    );
                }
            }
        }

        Self::with_manager(|m| {
            if let Some(manager) = m {
                manager.call_on_frame();
            }
        });

        // Broadcast physics frame tick to modular engine extensions
        crate::extension::extension_registry().on_server_frame();

        // Advance frame tick and continuous clock in TimerService
        static FRAME_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tick = FRAME_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let now = Self::current_time();
        let timer_svc = crate::timer::timer_service();
        let _ = timer_svc.tick(tick, now as f64, |token| {
            Self::with_sessions(|s| s.is_token_valid(token)).unwrap_or(false)
        });

        if let Some(engine) = Self::engine()
            && let Ok(mut mgr) = crate::menu::menu_manager().lock()
        {
            mgr.tick_frame(now, engine.as_ref());
        }

        // Record frame time for hardware diagnostics (FPS and jitter telemetry)
        crate::hardware::system_info().on_frame();

        // Throttle disk flushing to at most once every second to prevent per-frame I/O stalls
        static LAST_LOG_FLUSH: OnceLock<Mutex<Instant>> = OnceLock::new();
        let tracker = LAST_LOG_FLUSH.get_or_init(|| Mutex::new(Instant::now()));
        if let Ok(mut last) = tracker.try_lock()
            && last.elapsed() >= std::time::Duration::from_millis(1000)
        {
            *last = Instant::now();
            crate::logging::flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_event_variants() {
        let ev1 = HostEvent::ServerFrame;
        let ev2 = HostEvent::Player {
            slot: 5,
            event: PlayerEvent::Disconnect,
        };
        let ev3 = HostEvent::MapChange {
            map_name: "de_dust2",
        };
        let ev4 = HostEvent::CmdStart {
            slot: 3,
            buttons: 0x0001,
        };

        assert_eq!(ev1, HostEvent::ServerFrame);
        assert_eq!(ev1.name(), "server_frame");
        assert!(ev1.payload().is_empty());

        assert_eq!(ev2.name(), "client_disconnect");
        assert_eq!(&*ev2.payload(), &5i32.to_le_bytes());

        assert_eq!(ev3.name(), "map_change");
        assert_eq!(&*ev3.payload(), b"de_dust2");

        assert_eq!(ev4.name(), "cmd_start");
        assert_eq!(ev4.payload().len(), 8);

        let ev_menu = HostEvent::MenuSelect {
            player: 2,
            item_id: 101,
        };
        assert_eq!(ev_menu.name(), "menu_select");
        assert_eq!(ev_menu.payload().len(), 8);

        let ev_dmg = HostEvent::EntityTakeDamage {
            victim: 1,
            inflictor: 2,
            attacker: 2,
            damage: 25.5,
            bits_damage_type: 2,
            timing: crate::hooks::types::HookTiming::Pre,
        };
        assert_eq!(ev_dmg.name(), "entity_take_damage_pre");
        assert_eq!(ev_dmg.payload().len(), 20);

        let ev_kill = HostEvent::EntityKilled {
            victim: 1,
            attacker: 2,
            gib_mode: 0,
            timing: crate::hooks::types::HookTiming::Post,
        };
        assert_eq!(ev_kill.name(), "entity_killed_post");
        assert_eq!(ev_kill.payload().len(), 12);
    }
}
