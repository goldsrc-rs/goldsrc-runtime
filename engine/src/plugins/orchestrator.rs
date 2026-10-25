//! Plugin orchestrator implementation.
//!
//! Coordinates multi-layered plugin state synchronization across declarative
//! configuration (`plugins.toml`), reactive rules (`paused_plugins`), and administrator
//! manual overrides (`manual_overrides`).

use crate::plugins_config::PluginsConfig;
use goldsrc_host_wasm::PluginManager;
use std::collections::HashMap;

/// Orchestrator coordinating declarative configuration, reactive states, and lifecycle transitions.
#[derive(Default, Debug, Clone)]
pub struct PluginOrchestrator;

impl PluginOrchestrator {
    /// Synchronizes plugin lifecycle states (paused/resumed) across all 3 layers:
    /// 1. Manual administrator overrides (`manual_overrides` - highest priority)
    /// 2. Reactive rule evaluations (`reactive_overrides`)
    /// 3. Declarative settings from `plugins.toml` (`plugins_config` - default)
    ///
    /// After applying states, recalculates dependency graph resolution across all loaded plugins.
    pub fn sync_plugin_states(
        manager: &mut PluginManager,
        plugins_config: &PluginsConfig,
        reactive_overrides: &HashMap<String, bool>,
        manual_overrides: &HashMap<String, bool>,
    ) {
        let plugins_info = manager.get_plugins_info();
        for info in plugins_info {
            if let Some(&is_paused) = manual_overrides.get(&info.name) {
                let reason = if is_paused {
                    Some("manual administrator override".to_string())
                } else {
                    None
                };
                let _ = manager.pause_plugin_with_reason(&info.name, is_paused, reason);
            } else if let Some(is_paused) = reactive_overrides.get(&info.name) {
                let reason = if *is_paused {
                    Some("reactive rule".to_string())
                } else {
                    None
                };
                let _ = manager.pause_plugin_with_reason(&info.name, *is_paused, reason);
            } else {
                let disabled_reason = plugins_config.plugin_disabled_reason(&info.name);
                let is_paused = disabled_reason.is_some();
                let _ = manager.pause_plugin_with_reason(&info.name, is_paused, disabled_reason);
            }
        }

        manager.recalculate_dependency_states();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goldsrc_api::cvar::{CvarFlags, CvarPort};
    use goldsrc_api::entity::EntitySpawner;
    use goldsrc_spi::engine::{
        EngineConsole, EngineEntities, EngineExtensions, EngineMessages, EnginePhysics,
        EnginePrecache, EngineSound, TraceResult,
    };
    use std::sync::Arc;

    struct NoopEngine;
    impl EngineExtensions for NoopEngine {}
    impl EnginePrecache for NoopEngine {
        fn precache_model(&self, _s: &str) -> i32 {
            0
        }
        fn precache_sound(&self, _s: &str) -> i32 {
            0
        }
        fn precache_generic(&self, _s: &str) -> i32 {
            0
        }
    }
    impl EngineMessages for NoopEngine {
        fn message_begin(&self, _d: i32, _t: i32, _o: Option<[f32; 3]>, _e: Option<i32>) {}
        fn message_end(&self) {}
        fn write_byte(&self, _b: i32) {}
        fn write_char(&self, _c: i32) {}
        fn write_short(&self, _s: i32) {}
        fn write_long(&self, _l: i32) {}
        fn write_angle(&self, _a: f32) {}
        fn write_coord(&self, _c: f32) {}
        fn write_string(&self, _s: &str) {}
        fn write_entity(&self, _e: i32) {}
        fn reg_user_msg(&self, _n: &str, _s: i32) -> i32 {
            -1
        }
    }
    impl EntitySpawner for NoopEngine {
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
    impl EngineEntities for NoopEngine {
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
        fn player_team(&self, _index: i32) -> i32 {
            0
        }
        fn player_lang(&self, _index: i32) -> Option<String> {
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
    impl CvarPort for NoopEngine {
        fn cvar_get_string(&self, _n: &str) -> Option<String> {
            None
        }
        fn cvar_set_string(&self, _n: &str, _v: &str) {}
        fn cvar_get_float(&self, _n: &str) -> f32 {
            0.0
        }
        fn cvar_set_float(&self, _n: &str, _v: f32) {}
        fn cvar_register(&self, _name: &str, _default_value: &str, _flags: CvarFlags) -> bool {
            true
        }
    }
    impl EngineConsole for NoopEngine {
        fn server_command(&self, _cmd: &str) {}
        fn server_print(&self, _msg: &str) {}
        fn client_print(&self, _client_index: i32, _dest: i32, _message: &str) {}
    }
    impl EngineSound for NoopEngine {
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
    impl EnginePhysics for NoopEngine {
        fn trace_line(
            &self,
            _start: [f32; 3],
            _end: [f32; 3],
            _flags: i32,
            _skip_entity: i32,
        ) -> TraceResult {
            TraceResult {
                all_solid: false,
                start_solid: false,
                in_open: true,
                in_water: false,
                fraction: 1.0,
                end_pos: [0.0, 0.0, 0.0],
                plane_normal: [0.0, 0.0, 0.0],
                hit_entity: -1,
            }
        }
        fn trace_hull(
            &self,
            _start: [f32; 3],
            _end: [f32; 3],
            _flags: i32,
            _hull_number: i32,
            _skip_entity: i32,
        ) -> TraceResult {
            TraceResult {
                all_solid: false,
                start_solid: false,
                in_open: true,
                in_water: false,
                fraction: 1.0,
                end_pos: [0.0, 0.0, 0.0],
                plane_normal: [0.0, 0.0, 0.0],
                hit_entity: -1,
            }
        }
        fn point_contents(&self, _point: [f32; 3]) -> i32 {
            0
        }
    }

    #[test]
    fn test_plugin_orchestrator_sync_empty() {
        let mut manager = PluginManager::new(Arc::new(NoopEngine)).unwrap();
        let config = PluginsConfig::default();
        let reactive = HashMap::new();
        let manual = HashMap::new();

        PluginOrchestrator::sync_plugin_states(&mut manager, &config, &reactive, &manual);
        assert_eq!(manager.get_plugins_info().len(), 0);
    }
}
