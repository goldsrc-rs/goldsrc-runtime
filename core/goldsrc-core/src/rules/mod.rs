pub mod orchestrator;
pub use orchestrator::RuleOrchestrator;

use crate::plugins_config::PluginsConfig;
use goldsrc_api::consts::log_targets;
use goldsrc_api::rules::{RuleAction, RuleCondition, RuleScope};
use goldsrc_spi::engine::EnginePort;
use std::collections::HashMap;

/// Server context provided during rule evaluation.
pub struct ServerRuleContext<'a> {
    pub map_name: &'a str,
    pub player_count: usize,
    pub engine: &'a dyn EnginePort,
    pub plugins_config: &'a mut PluginsConfig,
    pub paused_plugins: &'a mut HashMap<String, bool>,
    /// Deliberate manual overrides initiated by administrator console commands (e.g. `grs pause`, `grs unpause`).
    pub manual_overrides: &'a HashMap<String, bool>,
    pub execution_log: Vec<String>,
}

// ----------------------------------------------------------------------------
// Built-in Conditions
// ----------------------------------------------------------------------------

/// Evaluates map name against strings, prefixes (`de_*`, `fy_*`), or exclusions (`!aim_*`).
pub struct MapCondition;

impl<'a> RuleCondition<ServerRuleContext<'a>> for MapCondition {
    fn name(&self) -> &str {
        "map"
    }

    fn scopes(&self) -> Vec<RuleScope> {
        vec![RuleScope::MapChange]
    }

    fn evaluate(&self, ctx: &ServerRuleContext<'a>, value: &toml::Value) -> bool {
        let current_map = ctx.map_name;

        let check_single = |pattern: &str| -> bool {
            if let Some(stripped) = pattern.strip_prefix('!') {
                // Inverted match
                if let Some(prefix) = stripped.strip_suffix('*') {
                    !current_map.starts_with(prefix)
                } else {
                    current_map != stripped
                }
            } else if let Some(prefix) = pattern.strip_suffix('*') {
                current_map.starts_with(prefix)
            } else {
                current_map.eq_ignore_ascii_case(pattern)
            }
        };

        match value {
            toml::Value::String(s) => check_single(s),
            toml::Value::Array(arr) => {
                // If any inclusion pattern matches (or exclusions pass)
                arr.iter().any(|v| match v {
                    toml::Value::String(s) => check_single(s),
                    _ => false,
                })
            }
            _ => false,
        }
    }
}

/// Evaluates player count against comparison strings (`>= 10`, `< 5`, `== 0`, `5..15`).
pub struct PlayersCondition;

impl<'a> RuleCondition<ServerRuleContext<'a>> for PlayersCondition {
    fn name(&self) -> &str {
        "players"
    }

    fn scopes(&self) -> Vec<RuleScope> {
        vec![RuleScope::PlayerCount]
    }

    fn evaluate(&self, ctx: &ServerRuleContext<'a>, value: &toml::Value) -> bool {
        let count = ctx.player_count;

        match value {
            toml::Value::Integer(n) => count >= *n as usize,
            toml::Value::String(expr) => {
                let expr = expr.trim();
                if let Some(val_str) = expr.strip_prefix(">=") {
                    val_str.trim().parse::<usize>().is_ok_and(|v| count >= v)
                } else if let Some(val_str) = expr.strip_prefix("<=") {
                    val_str.trim().parse::<usize>().is_ok_and(|v| count <= v)
                } else if let Some(val_str) = expr.strip_prefix('>') {
                    val_str.trim().parse::<usize>().is_ok_and(|v| count > v)
                } else if let Some(val_str) = expr.strip_prefix('<') {
                    val_str.trim().parse::<usize>().is_ok_and(|v| count < v)
                } else if let Some(val_str) = expr.strip_prefix("==") {
                    val_str.trim().parse::<usize>().is_ok_and(|v| count == v)
                } else if let Some((start, end)) = expr.split_once("..") {
                    let Ok(s) = start.trim().parse::<usize>() else {
                        log::warn!(
                            target: log_targets::RULES,
                            "Invalid start range in players condition: '{}'",
                            start
                        );
                        return false;
                    };
                    let Ok(e) = end.trim().parse::<usize>() else {
                        log::warn!(
                            target: log_targets::RULES,
                            "Invalid end range in players condition: '{}'",
                            end
                        );
                        return false;
                    };
                    count >= s && count <= e
                } else {
                    expr.parse::<usize>().is_ok_and(|v| count == v)
                }
            }
            _ => false,
        }
    }
}

/// Evaluates server CVar expressions (e.g. `mp_friendlyfire == 1`, `sv_gravity < 800`).
pub struct CvarCondition;

impl<'a> RuleCondition<ServerRuleContext<'a>> for CvarCondition {
    fn name(&self) -> &str {
        "cvar"
    }

    fn scopes(&self) -> Vec<RuleScope> {
        vec![RuleScope::MapChange]
    }

    fn evaluate(&self, ctx: &ServerRuleContext<'a>, value: &toml::Value) -> bool {
        let expr = match value {
            toml::Value::String(s) => s.as_str(),
            _ => return false,
        };

        // Parse operator
        let (cvar_name, op, expected_str) = if let Some((c, v)) = expr.split_once("==") {
            (c.trim(), "==", v.trim())
        } else if let Some((c, v)) = expr.split_once("!=") {
            (c.trim(), "!=", v.trim())
        } else if let Some((c, v)) = expr.split_once(">=") {
            (c.trim(), ">=", v.trim())
        } else if let Some((c, v)) = expr.split_once("<=") {
            (c.trim(), "<=", v.trim())
        } else if let Some((c, v)) = expr.split_once('>') {
            (c.trim(), ">", v.trim())
        } else if let Some((c, v)) = expr.split_once('<') {
            (c.trim(), "<", v.trim())
        } else {
            log::warn!(target: log_targets::RULES, "Invalid cvar expression operator: '{}'", expr);
            return false;
        };

        let Ok(expected_val) = expected_str.parse::<f32>() else {
            log::warn!(
                target: log_targets::RULES,
                "Invalid numeric value in cvar condition: '{}'",
                expected_str
            );
            return false;
        };
        let current_val = ctx.engine.cvar_get_float(cvar_name);

        match op {
            "==" => (current_val - expected_val).abs() < 0.001,
            "!=" => (current_val - expected_val).abs() >= 0.001,
            ">=" => current_val >= expected_val,
            "<=" => current_val <= expected_val,
            ">" => current_val > expected_val,
            "<" => current_val < expected_val,
            _ => false,
        }
    }
}

/// Evaluates if a plugin group is currently enabled (`group_enabled = "vip_pack"`).
pub struct GroupEnabledCondition;

impl<'a> RuleCondition<ServerRuleContext<'a>> for GroupEnabledCondition {
    fn name(&self) -> &str {
        "group_enabled"
    }

    fn scopes(&self) -> Vec<RuleScope> {
        vec![RuleScope::MapChange]
    }

    fn evaluate(&self, ctx: &ServerRuleContext<'a>, value: &toml::Value) -> bool {
        match value {
            toml::Value::String(group_name) => ctx
                .plugins_config
                .groups
                .get(group_name)
                .map(|g| g.enabled)
                .unwrap_or(false),
            _ => false,
        }
    }
}

// ----------------------------------------------------------------------------
// Built-in Actions
// ----------------------------------------------------------------------------

/// Action to pause one or more plugins (`pause = ["vip_menu", "vip_core"]`).
pub struct PauseAction;

impl<'a> RuleAction<ServerRuleContext<'a>> for PauseAction {
    fn name(&self) -> &str {
        "pause"
    }

    fn execute(&self, ctx: &mut ServerRuleContext<'a>, value: &toml::Value) -> Result<(), String> {
        let plugins_to_pause = match value {
            toml::Value::String(s) => vec![s.clone()],
            toml::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| match v {
                    toml::Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => return Err("Expected string or list of plugin names".to_string()),
        };

        for p in plugins_to_pause {
            if ctx.manual_overrides.contains_key(&p) {
                log::debug!(
                    target: log_targets::RULES,
                    "Skipping reactive pause on plugin '{}': protected by administrator manual override",
                    p
                );
                continue;
            }
            ctx.paused_plugins.insert(p.clone(), true);
            ctx.execution_log.push(format!("Paused plugin '{}'", p));
        }

        Ok(())
    }
}

/// Action to unpause one or more plugins (`unpause = ["vip_menu"]`).
pub struct UnpauseAction;

impl<'a> RuleAction<ServerRuleContext<'a>> for UnpauseAction {
    fn name(&self) -> &str {
        "unpause"
    }

    fn execute(&self, ctx: &mut ServerRuleContext<'a>, value: &toml::Value) -> Result<(), String> {
        let plugins_to_unpause = match value {
            toml::Value::String(s) => vec![s.clone()],
            toml::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| match v {
                    toml::Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => return Err("Expected string or list of plugin names".to_string()),
        };

        for p in plugins_to_unpause {
            if ctx.manual_overrides.contains_key(&p) {
                log::debug!(
                    target: log_targets::RULES,
                    "Skipping reactive unpause on plugin '{}': protected by administrator manual override",
                    p
                );
                continue;
            }
            ctx.paused_plugins.insert(p.clone(), false);
            ctx.execution_log.push(format!("Unpaused plugin '{}'", p));
        }

        Ok(())
    }
}

/// Action to enable a plugin group (`enable_group = "vip_pack"`).
pub struct EnableGroupAction;

impl<'a> RuleAction<ServerRuleContext<'a>> for EnableGroupAction {
    fn name(&self) -> &str {
        "enable_group"
    }

    fn execute(&self, ctx: &mut ServerRuleContext<'a>, value: &toml::Value) -> Result<(), String> {
        match value {
            toml::Value::String(group_name) => {
                if let Some(g) = ctx.plugins_config.groups.get_mut(group_name) {
                    g.enabled = true;
                    ctx.execution_log
                        .push(format!("Enabled plugin group '{}'", group_name));
                    Ok(())
                } else {
                    Err(format!(
                        "Group '{}' not found in plugins_config",
                        group_name
                    ))
                }
            }
            _ => Err("Expected string group name".to_string()),
        }
    }
}

/// Action to disable a plugin group (`disable_group = "vip_pack"`).
pub struct DisableGroupAction;

impl<'a> RuleAction<ServerRuleContext<'a>> for DisableGroupAction {
    fn name(&self) -> &str {
        "disable_group"
    }

    fn execute(&self, ctx: &mut ServerRuleContext<'a>, value: &toml::Value) -> Result<(), String> {
        match value {
            toml::Value::String(group_name) => {
                if let Some(g) = ctx.plugins_config.groups.get_mut(group_name) {
                    g.enabled = false;
                    ctx.execution_log
                        .push(format!("Disabled plugin group '{}'", group_name));
                    Ok(())
                } else {
                    Err(format!(
                        "Group '{}' not found in plugins_config",
                        group_name
                    ))
                }
            }
            _ => Err("Expected string group name".to_string()),
        }
    }
}

/// Action to set one or more server CVars (`set_cvar = { "sv_gravity" = 700 }`).
pub struct SetCvarAction;

impl<'a> RuleAction<ServerRuleContext<'a>> for SetCvarAction {
    fn name(&self) -> &str {
        "set_cvar"
    }

    fn execute(&self, ctx: &mut ServerRuleContext<'a>, value: &toml::Value) -> Result<(), String> {
        if let toml::Value::Table(table) = value {
            for (cvar_name, val) in table {
                let val_str = match val {
                    toml::Value::String(s) => s.clone(),
                    toml::Value::Integer(i) => i.to_string(),
                    toml::Value::Float(f) => f.to_string(),
                    toml::Value::Boolean(b) => if *b { "1" } else { "0" }.to_string(),
                    _ => continue,
                };
                ctx.engine.cvar_set_string(cvar_name, &val_str);
                ctx.execution_log
                    .push(format!("Set CVar '{}' to '{}'", cvar_name, val_str));
            }
            Ok(())
        } else {
            Err("Expected table of cvar_name = value".to_string())
        }
    }
}

/// Action to execute server console commands (`exec = "server_night.cfg"`).
pub struct ExecAction;

impl<'a> RuleAction<ServerRuleContext<'a>> for ExecAction {
    fn name(&self) -> &str {
        "exec"
    }

    fn execute(&self, ctx: &mut ServerRuleContext<'a>, value: &toml::Value) -> Result<(), String> {
        match value {
            toml::Value::String(cmd) => {
                ctx.engine.server_command(cmd);
                ctx.execution_log
                    .push(format!("Executed command '{}'", cmd));
                Ok(())
            }
            _ => Err("Expected string command".to_string()),
        }
    }
}

/// Creates a default `RuleRegistry` populated with all built-in server conditions and actions.
pub fn create_default_server_rule_registry<'a>()
-> goldsrc_api::rules::RuleRegistry<ServerRuleContext<'a>> {
    let mut registry = goldsrc_api::rules::RuleRegistry::new();
    registry.register_condition(MapCondition);
    registry.register_condition(PlayersCondition);
    registry.register_condition(CvarCondition);
    registry.register_condition(GroupEnabledCondition);
    registry.register_action(PauseAction);
    registry.register_action(UnpauseAction);
    registry.register_action(EnableGroupAction);
    registry.register_action(DisableGroupAction);
    registry.register_action(SetCvarAction);
    registry.register_action(ExecAction);
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use goldsrc_api::cvar::{CvarFlags, CvarPort};
    use goldsrc_api::entity::EntitySpawner;
    use goldsrc_api::rules::Rule;
    use goldsrc_spi::engine::{
        EngineConsole, EngineEntities, EngineExtensions, EngineMessages, EnginePhysics,
        EnginePrecache, EngineSound, TraceResult,
    };

    struct MockEngine;
    impl EngineExtensions for MockEngine {}
    impl EnginePrecache for MockEngine {
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
    impl EngineMessages for MockEngine {
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
            0
        }
    }
    impl EntitySpawner for MockEngine {
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
    impl EngineEntities for MockEngine {
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
    impl CvarPort for MockEngine {
        fn cvar_get_float(&self, _n: &str) -> f32 {
            0.0
        }
        fn cvar_set_float(&self, _n: &str, _v: f32) {}
        fn cvar_get_string(&self, _n: &str) -> Option<String> {
            None
        }
        fn cvar_set_string(&self, _n: &str, _v: &str) {}
        fn cvar_register(&self, _name: &str, _default_value: &str, _flags: CvarFlags) -> bool {
            true
        }
    }
    impl EnginePhysics for MockEngine {
        fn point_contents(&self, _point: [f32; 3]) -> i32 {
            0
        }
        fn trace_line(&self, _s: [f32; 3], _e: [f32; 3], _f: i32, _i: i32) -> TraceResult {
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
        fn trace_hull(&self, _s: [f32; 3], _e: [f32; 3], _f: i32, _h: i32, _i: i32) -> TraceResult {
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
    }
    impl EngineSound for MockEngine {
        fn emit_sound(&self, _e: i32, _c: i32, _s: &str, _v: f32, _a: f32, _f: i32, _p: i32) {}
        fn emit_ambient_sound(
            &self,
            _e: i32,
            _pos: [f32; 3],
            _s: &str,
            _v: f32,
            _a: f32,
            _f: i32,
            _p: i32,
        ) {
        }
    }
    impl EngineConsole for MockEngine {
        fn server_print(&self, _m: &str) {}
        fn client_print(&self, _c: i32, _t: i32, _m: &str) {}
        fn server_command(&self, _c: &str) {}
    }

    #[test]
    fn test_server_rule_execution() {
        let mut cfg = PluginsConfig::default();
        let mut paused = HashMap::new();
        let manual_overrides = HashMap::new();
        let mock_engine = MockEngine;

        {
            let registry = create_default_server_rule_registry();

            let mut when = std::collections::BTreeMap::new();
            when.insert("map".to_string(), toml::Value::String("de_*".to_string()));
            when.insert(
                "players".to_string(),
                toml::Value::String(">= 10".to_string()),
            );

            let mut action = std::collections::BTreeMap::new();
            action.insert(
                "pause".to_string(),
                toml::Value::Array(vec![toml::Value::String("warmup_mod".to_string())]),
            );

            let rule = Rule::new("auto_pause_warmup", when, action);
            let engine = goldsrc_api::rules::RuleEngine::new(registry, vec![rule]);

            let mut ctx = ServerRuleContext {
                map_name: "de_dust2",
                player_count: 12,
                engine: &mock_engine,
                plugins_config: &mut cfg,
                paused_plugins: &mut paused,
                manual_overrides: &manual_overrides,
                execution_log: Vec::new(),
            };

            let res = engine.evaluate_and_execute(&mut ctx);
            assert_eq!(res.len(), 1);
            assert!(res[0].1.is_ok());
        }

        assert_eq!(paused.get("warmup_mod"), Some(&true));
    }

    #[test]
    fn test_rule_orchestrator_scoping_and_manual_overrides() {
        let mut cfg = PluginsConfig::default();
        let mut paused = HashMap::new();
        let mock_engine = MockEngine;

        let mut when_map = std::collections::BTreeMap::new();
        when_map.insert(
            "map".to_string(),
            toml::Value::String("de_dust2".to_string()),
        );
        let mut action_map = std::collections::BTreeMap::new();
        action_map.insert(
            "pause".to_string(),
            toml::Value::Array(vec![toml::Value::String("vip_menu".to_string())]),
        );
        let map_rule = Rule::new("auto_pause_vip_on_dust2", when_map, action_map);

        let mut when_players = std::collections::BTreeMap::new();
        when_players.insert(
            "players".to_string(),
            toml::Value::String(">= 4".to_string()),
        );
        let mut action_players = std::collections::BTreeMap::new();
        action_players.insert(
            "unpause".to_string(),
            toml::Value::Array(vec![toml::Value::String("test_hud".to_string())]),
        );
        let players_rule = Rule::new("unpause_test_hud_on_dust2", when_players, action_players);

        let mut orchestrator = RuleOrchestrator::new();
        orchestrator.set_rules(vec![map_rule, players_rule]);

        // 1. When a player connects (RuleScope::PlayerCount):
        // The map rule must NOT run, so vip_menu is not touched.
        {
            let manual = orchestrator.manual_overrides().clone();
            let mut ctx = ServerRuleContext {
                map_name: "de_dust2",
                player_count: 5,
                engine: &mock_engine,
                plugins_config: &mut cfg,
                paused_plugins: &mut paused,
                manual_overrides: &manual,
                execution_log: Vec::new(),
            };

            let results = orchestrator.evaluate_scope(&RuleScope::PlayerCount, &mut ctx);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].0, "unpause_test_hud_on_dust2");
            assert_eq!(paused.get("test_hud"), Some(&false));
            assert_eq!(paused.get("vip_menu"), None); // Map rule was skipped!
        }

        // 2. When server changes map (RuleScope::MapChange):
        // Map rule executes and pauses vip_menu.
        {
            let manual = orchestrator.manual_overrides().clone();
            let mut ctx = ServerRuleContext {
                map_name: "de_dust2",
                player_count: 5,
                engine: &mock_engine,
                plugins_config: &mut cfg,
                paused_plugins: &mut paused,
                manual_overrides: &manual,
                execution_log: Vec::new(),
            };

            let results = orchestrator.evaluate_scope(&RuleScope::MapChange, &mut ctx);
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].0, "auto_pause_vip_on_dust2");
            assert_eq!(paused.get("vip_menu"), Some(&true));
        }

        // 3. Administrator deliberately runs `grs unpause vip_menu` -> sets manual override:
        orchestrator.set_manual_override("vip_menu", false);
        paused.insert("vip_menu".to_string(), false);

        // 4. Now another player connects (or even full scope re-evaluates):
        // Because of manual override, vip_menu is NOT re-paused!
        {
            let manual = orchestrator.manual_overrides().clone();
            let mut ctx = ServerRuleContext {
                map_name: "de_dust2",
                player_count: 6,
                engine: &mock_engine,
                plugins_config: &mut cfg,
                paused_plugins: &mut paused,
                manual_overrides: &manual,
                execution_log: Vec::new(),
            };

            let results = orchestrator.evaluate_scope(&RuleScope::All, &mut ctx);
            assert_eq!(results.len(), 2);
            // vip_menu should remain false because manual_override protected it!
            assert_eq!(paused.get("vip_menu"), Some(&false));
        }

        // 5. On map change, manual overrides reset
        orchestrator.on_map_change();
        assert!(orchestrator.manual_overrides().is_empty());
    }
}
