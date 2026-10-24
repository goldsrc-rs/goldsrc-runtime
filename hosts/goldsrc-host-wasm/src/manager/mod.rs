//! WASM plugin manager façade.
//!
//! Handles loading, lifecycle, tick dispatch, capability tracking, storage sandbox,
//! and hot-reloading of WebAssembly plugins via Wasmtime Component Model.

pub mod commands;
pub mod lifecycle;
pub mod loader;
pub mod state;

pub use commands::CommandRegistry;
pub use state::HostState;

use crate::error::{CommandError, LoadError};
use crate::plugin::{LoadedPlugin, PluginMetadata, PluginStatus};
use goldsrc_spi::engine::EnginePort as GoldsrcEngine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use wasmtime::{Config, Engine};

/// Read-only snapshot of a loaded plugin, used for CLI listing/info.
pub struct PluginInfo {
    /// Plugin name.
    pub name: String,
    /// Path the plugin was loaded from.
    pub path: PathBuf,
    /// Index in the plugin list.
    pub index: usize,
    /// Current lifecycle status.
    pub status: PluginStatus,
    /// Parsed metadata, if any.
    pub metadata: Option<PluginMetadata>,
    /// Whether the plugin exports `on_load`.
    pub has_on_load: bool,
    /// Whether the plugin exports `on_unload`.
    pub has_on_unload: bool,
    /// Whether the plugin exports `on_frame`.
    pub has_on_frame: bool,
}

/// Outcome of a pause/unpause operation on a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseOutcome {
    /// The plugin was successfully paused.
    Paused { name: String },
    /// The plugin was already paused.
    AlreadyPaused { name: String },
    /// The plugin was successfully resumed (unpaused).
    Resumed { name: String },
    /// The plugin was already running (not paused).
    AlreadyRunning { name: String },
}

impl PauseOutcome {
    /// Returns the target plugin's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Paused { name }
            | Self::AlreadyPaused { name }
            | Self::Resumed { name }
            | Self::AlreadyRunning { name } => name,
        }
    }

    /// Returns `true` if an actual lifecycle state transition occurred.
    pub fn changed(&self) -> bool {
        matches!(self, Self::Paused { .. } | Self::Resumed { .. })
    }
}

impl std::fmt::Display for PauseOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paused { name } => write!(f, "Plugin '{name}' paused successfully."),
            Self::AlreadyPaused { name } => write!(f, "Plugin '{name}' is already paused."),
            Self::Resumed { name } => write!(f, "Plugin '{name}' resumed successfully."),
            Self::AlreadyRunning { name } => write!(f, "Plugin '{name}' is already active."),
        }
    }
}

/// Aggregate outcome of pausing or resuming all loaded plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PauseAllOutcome {
    /// Number of plugins whose pause state actually changed.
    pub changed: usize,
    /// Number of plugins that were already in the requested state.
    pub already_in_state: usize,
    /// Target pause state (`true` = paused, `false` = resumed).
    pub pause: bool,
}

impl std::fmt::Display for PauseAllOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.pause {
            if self.already_in_state > 0 {
                write!(
                    f,
                    "Paused {} plugin(s) ({} were already paused).",
                    self.changed, self.already_in_state
                )
            } else {
                write!(f, "Paused {} plugin(s).", self.changed)
            }
        } else if self.already_in_state > 0 {
            write!(
                f,
                "Resumed {} plugin(s) ({} were already active).",
                self.changed, self.already_in_state
            )
        } else {
            write!(f, "Resumed {} plugin(s).", self.changed)
        }
    }
}

/// Central WASM runtime manager: loads components, executes lifecycle hooks,
pub struct PluginManager {
    pub(crate) plugins: Vec<LoadedPlugin>,
    pub(crate) engine: Engine,
    pub(crate) engine_ops: Arc<dyn GoldsrcEngine>,
    pub command_registry: CommandRegistry,
    pub(crate) plugin_dirs: Vec<PathBuf>,
}

impl PluginManager {
    /// Initialises the Wasmtime engine with the Component Model, epoch interruption,
    /// and guest backtrace formatting enabled.
    pub fn new(engine_ops: Arc<dyn GoldsrcEngine>) -> wasmtime::Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        // Note: Do NOT enable `config.debug_info(true)`. Cranelift cannot emit native host DWARF
        // information on 32-bit Windows MSVC (COFF/PE) targets, causing component compilation
        // to fail immediately with "failed to emit DWARF debug information".
        // Backtrace symbolication (file & line numbers) is handled by `wasm_backtrace_details(Enable)`.
        config.wasm_backtrace_details(wasmtime::WasmBacktraceDetails::Enable);
        let engine = Engine::new(&config)?;

        // Spawn background epoch timer thread to advance epochs every 2ms.
        let engine_clone = engine.clone();
        std::thread::Builder::new()
            .name("goldsrc-epoch-timer".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(2));
                    engine_clone.increment_epoch();
                }
            })
            .ok();

        Ok(Self {
            plugins: Vec::new(),
            engine,
            engine_ops,
            command_registry: CommandRegistry::new(),
            plugin_dirs: Vec::new(),
        })
    }

    /// Sets the list of base search directories for resolving plugin paths.
    pub fn set_plugin_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.plugin_dirs = dirs;
    }

    /// Sets the list of base search directories (builder style).
    pub fn with_plugin_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.plugin_dirs = dirs;
        self
    }

    /// Appends a plugin search directory.
    pub fn add_plugin_dir(&mut self, dir: PathBuf) {
        self.plugin_dirs.push(dir);
    }

    /// Returns a clone of the underlying Wasmtime Engine.
    pub fn wasm_engine(&self) -> Engine {
        self.engine.clone()
    }

    /// Returns a clone of the GoldsrcEngine ops trait object.
    pub fn engine_ops(&self) -> Arc<dyn GoldsrcEngine> {
        self.engine_ops.clone()
    }

    /// Compiles and instantiates a WASM plugin component without registering or running `on_load`.
    pub fn instantiate_plugin<P: AsRef<Path>>(&self, path: P) -> Result<LoadedPlugin, LoadError> {
        loader::instantiate_plugin(&self.engine, &self.engine_ops, path)
    }

    /// Loads a WASM plugin from `path`. Accepts either a pre-compiled
    /// component (magic `\0asm`) or a plain core module.
    pub fn load_plugin<P: AsRef<Path>>(&mut self, path: P) -> Result<String, LoadError> {
        let path = path.as_ref();
        let name_stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        if let Some(p) = self
            .plugins
            .iter()
            .find(|p| p.path == path || p.name == name_stem)
        {
            if p.status != PluginStatus::Unloaded {
                return Err(LoadError::AlreadyLoaded(p.name.clone()));
            }
        }

        let mut plugin = self.instantiate_plugin(path)?;
        plugin
            .call_on_load()
            .map_err(|e| LoadError::LoadPanic(e.to_string()))?;
        crate::host_log(&format!("Loaded component plugin: {}", plugin.name));
        let idx = self.plugins.len();
        if let Some(meta) = &plugin.metadata {
            self.command_registry.register_commands(idx, &meta.commands);
        }
        let name = plugin.name.clone();
        self.plugins.push(plugin);
        self.recalculate_dependency_states();
        Ok(name)
    }

    /// Resolves a plugin query (either numeric index or name) to a plugin index with strict bounds checking.
    ///
    /// If the query parses as an integer but is out of bounds, returns `CommandError::IndexOutOfBounds`.
    /// If the query does not match any plugin name, returns `CommandError::NotFound`.
    pub fn resolve_plugin_index(&self, query: &str) -> Result<usize, CommandError> {
        if let Ok(idx) = query.parse::<usize>() {
            if idx < self.plugins.len() {
                return Ok(idx);
            } else {
                return Err(CommandError::IndexOutOfBounds {
                    index: idx,
                    total: self.plugins.len(),
                });
            }
        }
        self.plugins
            .iter()
            .position(|p| p.name == query)
            .ok_or_else(|| CommandError::NotFound(query.to_string()))
    }

    /// Resolves a plugin query (either numeric index or name) to an optional plugin index.
    pub fn find_plugin(&self, query: &str) -> Option<usize> {
        self.resolve_plugin_index(query).ok()
    }

    /// Recalculates `status` across all loaded plugins according to dependency states.
    pub fn recalculate_dependency_states(&mut self) {
        lifecycle::recalculate_dependency_states(&mut self.plugins, &self.engine_ops);
    }

    /// Calls `on_unload` (if exported) and removes the plugin at `idx`.
    fn unload_plugin_at(&mut self, idx: usize) -> LoadedPlugin {
        let meta = self.plugins[idx].metadata.clone();
        if let Some(meta) = &meta {
            self.command_registry
                .unregister_commands(idx, &meta.commands);
        }
        let mut plugin = self.plugins.remove(idx);
        if plugin.has_export("on-unload") {
            let _ = plugin.call_on_unload();
        }
        self.command_registry.reindex_after_removal(idx);
        self.recalculate_dependency_states();
        plugin
    }

    /// Unloads all loaded plugins and returns a summary message.
    pub fn unload_all_plugins(&mut self) -> String {
        let count = self.plugins.len();
        while !self.plugins.is_empty() {
            self.unload_plugin_at(self.plugins.len() - 1);
        }
        format!("Unloaded {} plugins.", count)
    }

    /// Sets or clears the pause flag on a plugin by name or index query.
    pub fn pause_plugin(&mut self, query: &str, pause: bool) -> Result<PauseOutcome, CommandError> {
        self.pause_plugin_with_reason(query, pause, None)
    }

    /// Sets or clears the pause flag on a plugin with a descriptive reason.
    pub fn pause_plugin_with_reason(
        &mut self,
        query: &str,
        pause: bool,
        reason: Option<String>,
    ) -> Result<PauseOutcome, CommandError> {
        let idx = self.resolve_plugin_index(query)?;
        let name = self.plugins[idx].name.clone();
        let outcome = if pause {
            if matches!(self.plugins[idx].status, PluginStatus::Paused { .. }) {
                PauseOutcome::AlreadyPaused { name }
            } else {
                self.plugins[idx].status = PluginStatus::Paused { reason };
                self.recalculate_dependency_states();
                PauseOutcome::Paused { name }
            }
        } else if matches!(self.plugins[idx].status, PluginStatus::Paused { .. }) {
            self.plugins[idx].status = PluginStatus::Running;
            self.recalculate_dependency_states();
            PauseOutcome::Resumed { name }
        } else {
            PauseOutcome::AlreadyRunning { name }
        };
        Ok(outcome)
    }

    /// Sets or clears the pause flag on every loaded plugin.
    pub fn pause_all_plugins(&mut self, pause: bool) -> PauseAllOutcome {
        let mut changed = 0;
        let mut already_in_state = 0;
        for p in &mut self.plugins {
            if pause {
                if matches!(p.status, PluginStatus::Paused { .. }) {
                    already_in_state += 1;
                } else {
                    p.status = PluginStatus::Paused { reason: None };
                    changed += 1;
                }
            } else if matches!(p.status, PluginStatus::Paused { .. }) {
                p.status = PluginStatus::Running;
                changed += 1;
            } else {
                already_in_state += 1;
            }
        }
        if changed > 0 {
            self.recalculate_dependency_states();
        }
        PauseAllOutcome {
            changed,
            already_in_state,
            pause,
        }
    }

    /// Returns a snapshot of metadata for all loaded plugins.
    pub fn get_plugins_info(&self) -> Vec<PluginInfo> {
        self.plugins
            .iter()
            .enumerate()
            .map(|(index, p)| PluginInfo {
                name: p.name.clone(),
                path: p.path.clone(),
                index,
                status: p.status.clone(),
                metadata: p.metadata.clone(),
                has_on_load: p.has_export("on-load"),
                has_on_unload: p.has_export("on-unload"),
                has_on_frame: p.has_export("on-frame"),
            })
            .collect()
    }

    /// Unloads and reloads every loaded plugin from its recorded path.
    pub fn reload_all_plugins(&mut self) -> String {
        let paths: Vec<PathBuf> = self.plugins.iter().map(|p| p.path.clone()).collect();
        let count = paths.len();
        while !self.plugins.is_empty() {
            self.unload_plugin_at(self.plugins.len() - 1);
        }
        let mut failed = 0;
        for path in &paths {
            if self.load_plugin(path).is_err() {
                failed += 1;
            }
        }
        format!("Reloaded {} plugins ({} failed).", count - failed, failed)
    }

    /// Reloads a single plugin by name or index.
    pub fn reload_plugin_by_query(&mut self, query: &str) -> Result<String, CommandError> {
        let idx = self.resolve_plugin_index(query)?;
        let path = self.plugins[idx].path.clone();
        let name = self.plugins[idx].name.clone();
        self.unload_plugin_at(idx);
        self.load_plugin(&path)
            .map(|_| format!("Reloaded '{}'", name))
            .map_err(|source| CommandError::Load { name, source })
    }

    /// Unloads a plugin matching `path` if currently loaded (e.g. when its file is removed from disk).
    pub fn unload_plugin_by_path(&mut self, path: &Path) -> Option<String> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(idx) = self
            .plugins
            .iter()
            .position(|p| p.path == path || p.path.canonicalize().is_ok_and(|c| c == canonical))
        {
            let name = self.plugins[idx].name.clone();
            self.unload_plugin_at(idx);
            crate::host_log(&format!(
                "Plugin '{name}' file was deleted; unloaded plugin"
            ));
            Some(name)
        } else {
            None
        }
    }

    /// Commits a pre-compiled and instantiated plugin into the manager.
    /// Executes `on_load`, unloads any previous version at matching path, and registers commands.
    pub fn commit_reloaded_plugin(
        &mut self,
        mut new_plugin: LoadedPlugin,
        path: &Path,
    ) -> Result<String, LoadError> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let name = new_plugin.name.clone();

        if let Err(e) = new_plugin.call_on_load() {
            crate::host_log(&format!("Hot-reload on_load of '{}' failed: {e}", name));
            return Err(LoadError::LoadPanic(e.to_string()));
        }

        if let Some(idx) = self
            .plugins
            .iter()
            .position(|p| p.path == path || p.path.canonicalize().is_ok_and(|c| c == canonical))
        {
            self.unload_plugin_at(idx);
        }

        let new_idx = self.plugins.len();
        if let Some(meta) = &new_plugin.metadata {
            self.command_registry
                .register_commands(new_idx, &meta.commands);
        }
        self.plugins.push(new_plugin);
        self.recalculate_dependency_states();
        crate::host_log(&format!("Hot-reloaded plugin '{}'", name));
        Ok(name)
    }

    /// Reloads the plugin whose recorded path matches `path`, or loads it if newly discovered.
    pub fn reload_plugin_path(&mut self, path: &Path) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(idx) = self
            .plugins
            .iter()
            .position(|p| p.path == path || p.path.canonicalize().is_ok_and(|c| c == canonical))
        {
            let name = self.plugins[idx].name.clone();
            let old_path = self.plugins[idx].path.clone();

            if !path.exists() && !old_path.exists() {
                self.unload_plugin_at(idx);
                crate::host_log(&format!(
                    "Plugin '{name}' file was deleted; unloaded plugin"
                ));
                return;
            }

            match self.instantiate_plugin(&old_path) {
                Ok(mut new_plugin) => {
                    if let Err(e) = new_plugin.call_on_load() {
                        crate::host_log(&format!("Hot-reload on_load of '{}' failed: {e}", name));
                        return;
                    }
                    self.unload_plugin_at(idx);
                    let new_idx = self.plugins.len();
                    if let Some(meta) = &new_plugin.metadata {
                        self.command_registry
                            .register_commands(new_idx, &meta.commands);
                    }
                    self.plugins.push(new_plugin);
                    self.recalculate_dependency_states();
                    crate::host_log(&format!("Hot-reloaded plugin '{}'", name));
                }
                Err(e) => {
                    crate::host_log(&format!(
                        "Hot-reload of '{}' failed (previous version kept active): {e}",
                        name
                    ));
                }
            }
        } else {
            if !path.exists() {
                return;
            }
            match self.load_plugin(path) {
                Ok(msg) => crate::host_log(&format!("Discovered and loaded new plugin: {msg}")),
                Err(e) => crate::host_log(&format!("Failed to load new plugin at {:?}: {e}", path)),
            }
        }
    }

    /// Returns all registered command names across all loaded plugins.
    pub fn registered_commands(&self) -> Vec<String> {
        self.command_registry.command_names()
    }

    /// Dispatches a server command to the plugins that registered it via SMA U-cycle pipeline.
    pub fn dispatch_command(&mut self, cmd: &str, caller: i32, args: &str) -> bool {
        if let Some(owners) = self.command_registry.get_handlers(cmd) {
            let mut ctx = crate::pipeline::CommandContext {
                cmd,
                caller,
                args,
                consumed: false,
            };
            crate::pipeline::execute_command_pipeline(&mut self.plugins, owners, &mut ctx)
        } else {
            false
        }
    }

    /// Ticks every plugin's `on_frame`.
    pub fn on_server_frame(&mut self) {
        self.call_on_frame();
    }

    /// Loads a plugin by filesystem path or plugin name.
    pub fn load_plugin_by_name(&mut self, query: &str) -> Result<String, LoadError> {
        let mut path = PathBuf::from(query);
        if !path.exists() {
            let wasm_ext = goldsrc_api::consts::WASM_EXT;
            let with_ext = if !query.ends_with(wasm_ext) {
                PathBuf::from(format!("{query}{wasm_ext}"))
            } else {
                path.clone()
            };

            if with_ext.exists() {
                path = with_ext;
            } else {
                let mut found_path = None;
                for base_dir in &self.plugin_dirs {
                    let candidate = base_dir.join(query);
                    if candidate.exists() {
                        found_path = Some(candidate);
                        break;
                    }
                    let candidate_wasm = base_dir.join(format!("{query}{wasm_ext}"));
                    if candidate_wasm.exists() {
                        found_path = Some(candidate_wasm);
                        break;
                    }
                }

                if let Some(found) = found_path {
                    path = found;
                } else if !query.ends_with(wasm_ext) {
                    path = with_ext;
                }
            }
        }
        self.load_plugin(path)
    }

    /// Unloads a single plugin by name or index.
    pub fn unload_plugin_by_query(&mut self, query: &str) -> Result<String, CommandError> {
        let idx = self.resolve_plugin_index(query)?;
        let plugin = self.unload_plugin_at(idx);
        Ok(format!("Unloaded '{}'", plugin.name))
    }

    /// Returns the number of loaded plugins.
    pub fn loaded_count(&self) -> usize {
        self.plugins.len()
    }

    /// Calls `on_frame` on every (non-paused) plugin.
    pub fn call_on_frame(&mut self) {
        let mut had_poison = false;
        for plugin in &mut self.plugins {
            if plugin.call_on_frame().is_err() {
                had_poison = true;
            }
        }
        if had_poison {
            self.recalculate_dependency_states();
        }
    }

    /// Calls `on_event` on every (non-paused) plugin via SMA broadcast pipeline.
    pub fn call_on_event(&mut self, name: &str, data: &[u8]) {
        let mut ctx = crate::pipeline::EventContext {
            name,
            data,
            had_poison: false,
            delivered_count: 0,
        };
        crate::pipeline::execute_event_pipeline(&mut self.plugins, &mut ctx);
        if ctx.had_poison {
            self.recalculate_dependency_states();
        }
    }

    /// Calls `on_event` on a specific target plugin by name. Returns `true` if plugin was found and called.
    pub fn call_plugin_event(&mut self, target: &str, name: &str, data: &[u8]) -> bool {
        if let Some(plugin) = self.plugins.iter_mut().find(|p| p.name == target) {
            let _ = plugin.call_on_event(name, data);
            true
        } else {
            false
        }
    }

    /// Dispatches a placeholder resolution request to the owning WASM plugin.
    pub fn dispatch_placeholder(&mut self, name: &str, caller: i32, param: &str) -> Option<String> {
        let plugin_name = {
            let lock = WASM_PLACEHOLDERS.read().ok()?;
            lock.get(name)?.clone()
        };
        let plugin = self.plugins.iter_mut().find(|p| p.name == plugin_name)?;
        plugin
            .call_on_placeholder(name, caller, param)
            .ok()
            .flatten()
    }

    /// Dispatches chat message through loaded WASM plugins exporting on-chat.
    /// Returns Some(final_text) if accepted, or None if suppressed.
    pub fn dispatch_chat(&mut self, sender: i32, text: &str, is_team: bool) -> Option<String> {
        let mut current_text = text.to_string();
        let mut ctx = crate::pipeline::ChatContext {
            sender,
            is_team,
            current_text: &mut current_text,
        };
        let intent = crate::pipeline::ChatIntent { text };

        match crate::pipeline::execute_chat_pipeline(&mut self.plugins, &mut ctx, intent) {
            Ok(_) => Some(current_text),
            Err(crate::pipeline::ChatDropReason::SuppressedByPlugin(name)) => {
                log::trace!(
                    target: goldsrc_api::consts::log_targets::WASM,
                    "Chat message from sender {sender} suppressed by plugin '{name}'"
                );
                None
            }
            Err(_) => None,
        }
    }
}

static WASM_PLACEHOLDERS: std::sync::LazyLock<std::sync::RwLock<HashMap<String, String>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

/// Registers a custom placeholder mapped to its owning WASM plugin name.
pub fn register_host_placeholder(name: &str, plugin_name: &str) {
    if let Ok(mut lock) = WASM_PLACEHOLDERS.write() {
        lock.insert(name.to_string(), plugin_name.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::goldsrc::engine::api::Host;
    use goldsrc_api::cvar::{CvarFlags, CvarPort};
    use goldsrc_api::entity::EntitySpawner;
    use goldsrc_spi::engine::{
        EngineConsole, EngineEntities, EngineExtensions, EngineMessages, EnginePhysics,
        EnginePrecache, EngineSound, TraceResult,
    };

    struct NoopEngineOps;

    impl EngineExtensions for NoopEngineOps {}

    impl EnginePrecache for NoopEngineOps {
        fn precache_model(&self, _path: &str) -> i32 {
            0
        }
        fn precache_sound(&self, _path: &str) -> i32 {
            0
        }
        fn precache_generic(&self, _path: &str) -> i32 {
            0
        }
    }

    impl EngineMessages for NoopEngineOps {
        fn reg_user_msg(&self, _name: &str, _size: i32) -> i32 {
            0
        }

        fn message_begin(
            &self,
            _msg_dest: i32,
            _msg_type: i32,
            _origin: Option<[f32; 3]>,
            _edict_index: Option<i32>,
        ) {
        }
        fn message_end(&self) {}
        fn write_byte(&self, _val: i32) {}
        fn write_char(&self, _val: i32) {}
        fn write_short(&self, _val: i32) {}
        fn write_long(&self, _val: i32) {}
        fn write_angle(&self, _val: f32) {}
        fn write_coord(&self, _val: f32) {}
        fn write_string(&self, _val: &str) {}
        fn write_entity(&self, _val: i32) {}
    }

    impl EngineConsole for NoopEngineOps {
        fn server_print(&self, _message: &str) {}
        fn client_print(&self, _client_index: i32, _print_type: i32, _message: &str) {}
        fn server_command(&self, _command: &str) {}
    }

    impl EntitySpawner for NoopEngineOps {
        fn create_named_entity(&self, _classname: &str) -> Option<i32> {
            None
        }
        fn entity_set_origin(&self, _index: i32, _pos: [f32; 3]) {}
        fn entity_set_angles(&self, _index: i32, _angles: [f32; 3]) {}
        fn entity_key_value(&self, _index: i32, _key: &str, _value: &str) -> bool {
            false
        }
        fn dispatch_spawn(&self, _index: i32) -> i32 {
            0
        }
    }

    impl EngineEntities for NoopEngineOps {
        fn entity_is_valid(&self, _index: i32) -> bool {
            false
        }
        fn entity_classname(&self, _index: i32) -> Option<String> {
            None
        }
        fn entity_health(&self, _index: i32) -> f32 {
            0.0
        }
        fn entity_set_health(&self, _index: i32, _health: f32) {}
        fn entity_origin(&self, _index: i32) -> [f32; 3] {
            [0.0; 3]
        }
        fn entity_velocity(&self, _index: i32) -> [f32; 3] {
            [0.0; 3]
        }
        fn entity_set_velocity(&self, _index: i32, _vel: [f32; 3]) {}
        fn entity_angles(&self, _index: i32) -> [f32; 3] {
            [0.0; 3]
        }
        fn player_name(&self, _index: i32) -> Option<String> {
            None
        }
        fn player_armorvalue(&self, _index: i32) -> f32 {
            0.0
        }
        fn player_set_armorvalue(&self, _index: i32, _armor: f32) {}
        fn remove_entity(&self, _index: i32) {}
        fn drop_to_floor(&self, _index: i32) -> i32 {
            0
        }
        fn dispatch_touch(&self, _touched: i32, _other: i32) {}
    }

    impl CvarPort for NoopEngineOps {
        fn cvar_get_float(&self, _name: &str) -> f32 {
            0.0
        }
        fn cvar_set_float(&self, _name: &str, _val: f32) {}
        fn cvar_get_string(&self, _name: &str) -> Option<String> {
            None
        }
        fn cvar_set_string(&self, _name: &str, _val: &str) {}
        fn cvar_register(&self, _name: &str, _default_value: &str, _flags: CvarFlags) -> bool {
            true
        }
    }

    impl EnginePhysics for NoopEngineOps {
        fn point_contents(&self, _point: [f32; 3]) -> i32 {
            0
        }
        fn trace_line(
            &self,
            _start: [f32; 3],
            _end: [f32; 3],
            _flags: i32,
            _ignore_ent: i32,
        ) -> TraceResult {
            TraceResult::default()
        }
        fn trace_hull(
            &self,
            _start: [f32; 3],
            _end: [f32; 3],
            _flags: i32,
            _hull_number: i32,
            _ignore_ent: i32,
        ) -> TraceResult {
            TraceResult::default()
        }
    }

    impl EngineSound for NoopEngineOps {
        fn emit_sound(
            &self,
            _entity: i32,
            _channel: i32,
            _sample: &str,
            _volume: f32,
            _attenuation: f32,
            _flags: i32,
            _pitch: i32,
        ) {
        }
        fn emit_ambient_sound(
            &self,
            _entity: i32,
            _pos: [f32; 3],
            _sample: &str,
            _volume: f32,
            _attenuation: f32,
            _flags: i32,
            _pitch: i32,
        ) {
        }
    }

    /// Loads the built demo plugin and checks the command registry + consume semantics.
    /// Loads the built demo plugin and checks the command registry + consume semantics.
    #[test]
    fn command_registry_registers_and_consumes() {
        let candidates = [
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/wasm32-unknown-unknown/release/admin_system.wasm"
            ),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/wasm32-wasip1/debug/admin_system.wasm"
            ),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/wasm32-unknown-unknown/debug/admin_system.wasm"
            ),
        ];
        let wasm_path = candidates.iter().find(|p| std::path::Path::new(p).exists());
        let Some(&wasm_path) = wasm_path else {
            eprintln!("admin_system.wasm not built; skipping command registry test");
            return;
        };

        let mut manager = PluginManager::new(Arc::new(NoopEngineOps)).unwrap();
        manager.load_plugin(wasm_path).unwrap();

        let meta = &manager.plugins[0].metadata;
        assert!(meta.is_some());
        assert!(
            meta.as_ref()
                .unwrap()
                .commands
                .contains(&"admin_slay".to_string())
        );

        assert!(!manager.dispatch_command("nonexistent", 0, ""));
    }

    #[test]
    fn test_cwasm_aot_caching_and_reloading() {
        let candidates = [
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/wasm32-unknown-unknown/release/admin_system.wasm"
            ),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/wasm32-wasip1/debug/admin_system.wasm"
            ),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../target/wasm32-unknown-unknown/debug/admin_system.wasm"
            ),
        ];
        let src_path = candidates
            .iter()
            .map(std::path::Path::new)
            .find(|p| p.exists());

        let Some(src_path) = src_path else {
            eprintln!("admin_system.wasm not built; skipping cwasm AOT caching test");
            return;
        };

        let temp_dir = std::env::temp_dir().join(format!("grs_cwasm_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let plugin_wasm = temp_dir.join("admin_system.wasm");
        std::fs::copy(src_path, &plugin_wasm).unwrap();

        let manager = PluginManager::new(Arc::new(NoopEngineOps)).unwrap();

        // 1. Initial compilation: should populate .cache with .cwasm
        let loaded_1 = manager.instantiate_plugin(&plugin_wasm).unwrap();
        assert_eq!(loaded_1.name, "admin_system");

        let cache_dir = temp_dir.join(".cache");
        assert!(
            cache_dir.is_dir(),
            ".cache directory should have been created"
        );

        let cache_files: Vec<_> = std::fs::read_dir(&cache_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("cwasm"))
            .collect();
        assert_eq!(cache_files.len(), 1, "Expected exactly 1 cached cwasm file");
        let cached_cwasm_path = cache_files[0].path();

        // 2. Second load: should load from cache via deserialize_file
        let loaded_2 = manager.instantiate_plugin(&plugin_wasm).unwrap();
        assert_eq!(loaded_2.name, "admin_system");

        // 3. Direct loading of .cwasm file
        let loaded_3 = manager.instantiate_plugin(&cached_cwasm_path).unwrap();
        assert_eq!(
            loaded_3.name,
            cached_cwasm_path.file_stem().unwrap().to_str().unwrap()
        );

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[derive(Default)]
    struct MockMessageEngine {
        messages: std::sync::Mutex<Vec<(i32, i32, Option<i32>)>>,
        bytes: std::sync::Mutex<Vec<i32>>,
        strings: std::sync::Mutex<Vec<String>>,
        ended: std::sync::Mutex<usize>,
    }

    impl EnginePrecache for MockMessageEngine {
        fn precache_model(&self, _path: &str) -> i32 {
            0
        }
        fn precache_sound(&self, _path: &str) -> i32 {
            0
        }
        fn precache_generic(&self, _path: &str) -> i32 {
            0
        }
    }

    impl EngineMessages for MockMessageEngine {
        fn reg_user_msg(&self, _name: &str, _size: i32) -> i32 {
            75
        }
        fn message_begin(
            &self,
            msg_dest: i32,
            msg_type: i32,
            _origin: Option<[f32; 3]>,
            edict_index: Option<i32>,
        ) {
            self.messages
                .lock()
                .unwrap()
                .push((msg_dest, msg_type, edict_index));
        }
        fn message_end(&self) {
            *self.ended.lock().unwrap() += 1;
        }
        fn write_byte(&self, val: i32) {
            self.bytes.lock().unwrap().push(val);
        }
        fn write_char(&self, _val: i32) {}
        fn write_short(&self, _val: i32) {}
        fn write_long(&self, _val: i32) {}
        fn write_angle(&self, _val: f32) {}
        fn write_coord(&self, _val: f32) {}
        fn write_string(&self, val: &str) {
            self.strings.lock().unwrap().push(val.to_string());
        }
        fn write_entity(&self, _val: i32) {}
    }

    impl EngineConsole for MockMessageEngine {
        fn server_print(&self, _message: &str) {}
        fn client_print(&self, _client_index: i32, _print_type: i32, _message: &str) {}
        fn server_command(&self, _command: &str) {}
    }

    impl EntitySpawner for MockMessageEngine {
        fn create_named_entity(&self, _classname: &str) -> Option<i32> {
            None
        }
        fn entity_set_origin(&self, _index: i32, _pos: [f32; 3]) {}
        fn entity_set_angles(&self, _index: i32, _angles: [f32; 3]) {}
        fn entity_key_value(&self, _index: i32, _key: &str, _value: &str) -> bool {
            false
        }
        fn dispatch_spawn(&self, _index: i32) -> i32 {
            0
        }
    }

    impl EngineEntities for MockMessageEngine {
        fn entity_is_valid(&self, index: i32) -> bool {
            goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index)
        }
        fn entity_classname(&self, _index: i32) -> Option<String> {
            None
        }
        fn entity_health(&self, _index: i32) -> f32 {
            0.0
        }
        fn entity_set_health(&self, _index: i32, _health: f32) {}
        fn entity_origin(&self, _index: i32) -> [f32; 3] {
            [0.0; 3]
        }
        fn entity_velocity(&self, _index: i32) -> [f32; 3] {
            [0.0; 3]
        }
        fn entity_set_velocity(&self, _index: i32, _vel: [f32; 3]) {}
        fn entity_angles(&self, _index: i32) -> [f32; 3] {
            [0.0; 3]
        }
        fn player_name(&self, _index: i32) -> Option<String> {
            None
        }
        fn player_armorvalue(&self, _index: i32) -> f32 {
            0.0
        }
        fn player_set_armorvalue(&self, _index: i32, _armor: f32) {}
        fn remove_entity(&self, _index: i32) {}
        fn drop_to_floor(&self, _index: i32) -> i32 {
            0
        }
        fn dispatch_touch(&self, _touched: i32, _other: i32) {}
    }

    impl CvarPort for MockMessageEngine {
        fn cvar_get_float(&self, _name: &str) -> f32 {
            0.0
        }
        fn cvar_set_float(&self, _name: &str, _val: f32) {}
        fn cvar_get_string(&self, _name: &str) -> Option<String> {
            None
        }
        fn cvar_set_string(&self, _name: &str, _val: &str) {}
        fn cvar_register(&self, _name: &str, _default_value: &str, _flags: CvarFlags) -> bool {
            true
        }
    }

    impl EnginePhysics for MockMessageEngine {
        fn point_contents(&self, _point: [f32; 3]) -> i32 {
            0
        }
        fn trace_line(
            &self,
            _start: [f32; 3],
            _end: [f32; 3],
            _flags: i32,
            _ignore_ent: i32,
        ) -> TraceResult {
            TraceResult::default()
        }
        fn trace_hull(
            &self,
            _start: [f32; 3],
            _end: [f32; 3],
            _flags: i32,
            _hull_number: i32,
            _ignore_ent: i32,
        ) -> TraceResult {
            TraceResult::default()
        }
    }

    impl EngineSound for MockMessageEngine {
        fn emit_sound(
            &self,
            _entity: i32,
            _channel: i32,
            _sample: &str,
            _volume: f32,
            _attenuation: f32,
            _flags: i32,
            _pitch: i32,
        ) {
        }
        fn emit_ambient_sound(
            &self,
            _entity: i32,
            _pos: [f32; 3],
            _sample: &str,
            _volume: f32,
            _attenuation: f32,
            _flags: i32,
            _pitch: i32,
        ) {
        }
    }

    impl EngineExtensions for MockMessageEngine {}

    #[test]
    fn host_print_center_formats_and_dispatches_textmsg() {
        let engine = Arc::new(MockMessageEngine::default());
        let mut host_state = HostState {
            engine: engine.clone(),
            limits: wasmtime::StoreLimitsBuilder::new().build(),
            plugin_name: "test_plugin".to_string(),
            permissions: Vec::new(),
            shared_buckets: Vec::new(),
        };

        host_state.host_print_center(1, "Header\nDescription line".to_string());

        let messages = engine.messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0],
            (goldsrc_spi::engine::MessageDest::One as i32, 75, Some(1))
        );

        let bytes = engine.bytes.lock().unwrap().clone();
        assert_eq!(bytes, vec![goldsrc_api::HUD_PRINTCENTER]);

        let strings = engine.strings.lock().unwrap().clone();
        assert_eq!(strings, vec!["%s", "Header\rDescription line"]);

        assert_eq!(*engine.ended.lock().unwrap(), 1);
    }

    #[test]
    fn host_print_center_broadcast_to_all() {
        let engine = Arc::new(MockMessageEngine::default());
        let mut host_state = HostState {
            engine: engine.clone(),
            limits: wasmtime::StoreLimitsBuilder::new().build(),
            plugin_name: "test_plugin".to_string(),
            permissions: Vec::new(),
            shared_buckets: Vec::new(),
        };

        host_state.host_print_center(0, "Global center notice".to_string());

        let messages = engine.messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0],
            (goldsrc_spi::engine::MessageDest::All as i32, 75, None)
        );

        let bytes = engine.bytes.lock().unwrap().clone();
        assert_eq!(bytes, vec![goldsrc_api::HUD_PRINTCENTER]);

        let strings = engine.strings.lock().unwrap().clone();
        assert_eq!(strings, vec!["%s", "Global center notice"]);

        assert_eq!(*engine.ended.lock().unwrap(), 1);
    }

    #[test]
    fn test_active_menu_owner_tracking_and_lifecycle() {
        let engine = Arc::new(MockMessageEngine::default());
        let mut host_state = HostState {
            engine: engine.clone(),
            limits: wasmtime::StoreLimitsBuilder::new().build(),
            plugin_name: "test_menu".to_string(),
            permissions: Vec::new(),
            shared_buckets: Vec::new(),
        };

        crate::clear_all_active_menu_owners();
        assert_eq!(crate::get_active_menu_owner(1), None);

        // Opening menu sets owner
        host_state.host_show_menu(1, 0x3FF, -1, "Test Menu".to_string());
        assert_eq!(
            crate::get_active_menu_owner(1),
            Some("test_menu".to_string())
        );

        // Another plugin opening menu overrides owner
        let mut vip_state = HostState {
            engine: engine.clone(),
            limits: wasmtime::StoreLimitsBuilder::new().build(),
            plugin_name: "vip_menu".to_string(),
            permissions: Vec::new(),
            shared_buckets: Vec::new(),
        };
        vip_state.host_show_menu(1, 0x1F, -1, "VIP Menu".to_string());
        assert_eq!(
            crate::get_active_menu_owner(1),
            Some("vip_menu".to_string())
        );

        // Closing menu (keys_mask == 0) removes owner
        vip_state.host_show_menu(1, 0, 0, "".to_string());
        assert_eq!(crate::get_active_menu_owner(1), None);
    }

    #[test]
    fn test_pause_outcome_display_and_semantics() {
        let p = PauseOutcome::Paused {
            name: "vip_menu".to_string(),
        };
        assert!(p.changed());
        assert_eq!(p.name(), "vip_menu");
        assert_eq!(p.to_string(), "Plugin 'vip_menu' paused successfully.");

        let ap = PauseOutcome::AlreadyPaused {
            name: "vip_menu".to_string(),
        };
        assert!(!ap.changed());
        assert_eq!(ap.to_string(), "Plugin 'vip_menu' is already paused.");

        let r = PauseOutcome::Resumed {
            name: "vip_menu".to_string(),
        };
        assert!(r.changed());
        assert_eq!(r.to_string(), "Plugin 'vip_menu' resumed successfully.");

        let ar = PauseOutcome::AlreadyRunning {
            name: "vip_menu".to_string(),
        };
        assert!(!ar.changed());
        assert_eq!(ar.to_string(), "Plugin 'vip_menu' is already active.");

        let all_paused = PauseAllOutcome {
            changed: 3,
            already_in_state: 1,
            pause: true,
        };
        assert_eq!(
            all_paused.to_string(),
            "Paused 3 plugin(s) (1 were already paused)."
        );

        let all_resumed = PauseAllOutcome {
            changed: 4,
            already_in_state: 0,
            pause: false,
        };
        assert_eq!(all_resumed.to_string(), "Resumed 4 plugin(s).");
    }

    #[test]
    fn test_component_compilation_and_loading() {
        let wasm_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/wasm32-unknown-unknown/release/admin_system.wasm");
        if wasm_path.exists() {
            let engine = Arc::new(MockMessageEngine::default());
            let mut manager = PluginManager::new(engine).expect("failed to create PluginManager");
            let result = manager.load_plugin(&wasm_path);
            assert!(result.is_ok(), "Failed to load plugin: {:?}", result.err());
            assert_eq!(manager.plugins.len(), 1);
            assert_eq!(manager.plugins[0].name, "admin_system");
            assert_eq!(
                manager.plugins[0].status,
                crate::plugin::PluginStatus::Running
            );
            manager.call_on_frame();
            assert_eq!(
                manager.plugins[0].status,
                crate::plugin::PluginStatus::Running
            );
        }
    }
}
