//! Declarative `plugins.toml` Configuration Model & Parser.
//!
//! Provides granular plugin debugging, profile groups, and reactive rules.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Log level for plugin debugging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PluginLogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

/// Detailed debugging and profiling configuration for a plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginDebugConfig {
    /// Logging verbosity level.
    #[serde(default)]
    pub level: PluginLogLevel,
    /// Specific events to trace/log.
    #[serde(default)]
    pub log_events: Vec<String>,
    /// Watchdog epoch deadline override (default: 100 epochs = ~200ms).
    #[serde(default = "default_epoch_limit")]
    pub epoch_limit: u64,
    /// Dedicated log file for this plugin (in `logs/` directory).
    #[serde(default)]
    pub log_file: Option<String>,
    /// Whether to profile per-tick and event execution times.
    #[serde(default)]
    pub profile: bool,
    /// Whether to log command invocations and arguments.
    #[serde(default = "default_true")]
    pub log_commands: bool,
}

fn default_true() -> bool {
    true
}

fn default_epoch_limit() -> u64 {
    100
}

impl Default for PluginDebugConfig {
    fn default() -> Self {
        Self {
            level: PluginLogLevel::Info,
            profile: false,
            log_events: Vec::new(),
            log_commands: true,
            epoch_limit: 100,
            log_file: None,
        }
    }
}

/// Debug setting which can be either a simple boolean (`debug = true`) or a full table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginDebugSetting {
    Simple(bool),
    Detailed(PluginDebugConfig),
}

impl PluginDebugSetting {
    /// Resolves the setting into a full `PluginDebugConfig`.
    pub fn resolve(&self) -> PluginDebugConfig {
        match self {
            Self::Simple(enabled) => {
                if *enabled {
                    PluginDebugConfig {
                        level: PluginLogLevel::Debug,
                        profile: true,
                        ..Default::default()
                    }
                } else {
                    PluginDebugConfig::default()
                }
            }
            Self::Detailed(cfg) => cfg.clone(),
        }
    }
}

use goldsrc_api::dag::PluginTier;

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        One(String),
        Many(Vec<String>),
    }

    match Option::<StringOrVec>::deserialize(deserializer)? {
        Some(StringOrVec::One(s)) => Ok(vec![s]),
        Some(StringOrVec::Many(v)) => Ok(v),
        None => Ok(Vec::new()),
    }
}

/// Individual plugin entry in `plugins.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginEntry {
    /// Plugin name or relative path (e.g. "admin_system", "test_suite/test_hud").
    pub name: String,
    /// Architectural loading tier (Core -> Service -> Gameplay -> Addon -> Analytics).
    #[serde(default)]
    pub tier: PluginTier,
    /// Required dependencies (plugin names) that must load before this plugin.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub requires: Vec<String>,
    /// Debugging and profiling configuration.
    #[serde(default)]
    pub debug: Option<PluginDebugSetting>,
    /// Whether the plugin is enabled for loading.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// A named profile group of plugins (e.g. `[groups.vip_pack]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginGroup {
    /// List of plugin names or relative paths belonging to this group.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Whether the entire group is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// A reactive lifecycle rule in `plugins.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleConfig {
    /// Name or description of the rule.
    pub name: String,
    /// Condition map: condition name -> TOML value.
    #[serde(default)]
    pub when: BTreeMap<String, toml::Value>,
    /// Action map: action name -> TOML value.
    #[serde(default)]
    pub action: BTreeMap<String, toml::Value>,
    /// Explicit trigger scopes (e.g. `scope = "map_change"` or `scope = ["map_change", "player_count"]`).
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub scope: Vec<String>,
}

impl RuleConfig {
    /// Converts this config model into a [`goldsrc_api::rules::Rule`].
    pub fn to_rule(&self) -> goldsrc_api::rules::Rule {
        let scopes: Vec<goldsrc_api::rules::RuleScope> = self
            .scope
            .iter()
            .map(|s| goldsrc_api::rules::RuleScope::parse(s))
            .collect();
        goldsrc_api::rules::Rule::with_scopes(
            &self.name,
            self.when.clone(),
            self.action.clone(),
            scopes,
        )
    }
}

/// Individual plugin entry representation before resolving name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginEntryItem {
    /// Architectural loading tier (Core -> Service -> Gameplay -> Addon -> Analytics).
    #[serde(default)]
    pub tier: PluginTier,
    /// Required dependencies (plugin names) that must load before this plugin.
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub requires: Vec<String>,
    /// Debugging and profiling configuration.
    #[serde(default)]
    pub debug: Option<PluginDebugSetting>,
    /// Whether the plugin is enabled for loading.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Flexible representation of `plugins` section supporting both array and named table (map).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginsSection {
    Array(Vec<PluginEntry>),
    Map(BTreeMap<String, PluginEntryItem>),
}

/// Flexible representation of `rules` section supporting both array and named table (map).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RulesSection {
    Array(Vec<RuleConfig>),
    Map(BTreeMap<String, RuleItemConfig>),
}

/// A reactive lifecycle rule without explicit `name` field (key is name).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleItemConfig {
    /// Condition map: condition name -> TOML value.
    #[serde(default)]
    pub when: BTreeMap<String, toml::Value>,
    /// Action map: action name -> TOML value.
    #[serde(default)]
    pub action: BTreeMap<String, toml::Value>,
    /// Explicit trigger scopes (e.g. `scope = "map_change"` or `scope = ["map_change", "player_count"]`).
    #[serde(default, deserialize_with = "deserialize_string_or_vec")]
    pub scope: Vec<String>,
}

/// Intermediate raw TOML representation for flexible dual-format deserialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct RawPluginsConfig {
    #[serde(default)]
    plugins: Option<PluginsSection>,
    #[serde(default)]
    groups: BTreeMap<String, PluginGroup>,
    #[serde(default)]
    rules: Option<RulesSection>,
}

/// Root configuration schema for `plugins.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PluginsConfig {
    /// Explicit list of plugin declarations.
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
    /// Named profile groups (`[groups.<name>]`).
    #[serde(default)]
    pub groups: BTreeMap<String, PluginGroup>,
    /// Reactive lifecycle rules (`[[rules]]` or `[rules.<name>]`).
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
}

impl PluginsConfig {
    /// Parses a `plugins.toml` string supporting both Array-of-Tables and Named-Map formats.
    pub fn parse(toml_str: &str) -> Result<Self, toml::de::Error> {
        let raw: RawPluginsConfig = toml::from_str(toml_str)?;

        let mut plugins = Vec::new();
        if let Some(sec) = raw.plugins {
            match sec {
                PluginsSection::Array(arr) => plugins = arr,
                PluginsSection::Map(map) => {
                    for (name, item) in map {
                        plugins.push(PluginEntry {
                            name,
                            enabled: item.enabled,
                            tier: item.tier,
                            requires: item.requires,
                            debug: item.debug,
                        });
                    }
                }
            }
        }

        let mut rules = Vec::new();
        if let Some(sec) = raw.rules {
            match sec {
                RulesSection::Array(arr) => rules = arr,
                RulesSection::Map(map) => {
                    for (name, item) in map {
                        rules.push(RuleConfig {
                            name,
                            when: item.when,
                            action: item.action,
                            scope: item.scope,
                        });
                    }
                }
            }
        }

        Ok(Self {
            plugins,
            groups: raw.groups,
            rules,
        })
    }

    /// Checks whether a plugin is enabled, returning the reason if disabled.
    pub fn plugin_disabled_reason(&self, plugin_name: &str) -> Option<String> {
        let base_name = plugin_name
            .rsplit_once('/')
            .map(|(_, b)| b)
            .unwrap_or(plugin_name);

        // 1. If any group containing this plugin is explicitly disabled
        for (group_name, group) in &self.groups {
            if !group.enabled
                && group.plugins.iter().any(|p| {
                    p == plugin_name
                        || p == base_name
                        || p.rsplit_once('/').map(|(_, b)| b).unwrap_or(p) == base_name
                })
            {
                return Some(format!("group '{}' disabled", group_name));
            }
        }

        // 2. Check individual entry
        if let Some(entry) = self.plugins.iter().find(|p| {
            p.name == plugin_name
                || p.name == base_name
                || p.name.rsplit_once('/').map(|(_, b)| b).unwrap_or(&p.name) == base_name
        }) && !entry.enabled
        {
            return Some(format!("plugin '{}' disabled in config", entry.name));
        }

        None
    }

    /// Checks whether a plugin is enabled, taking both individual entry and groups into account.
    pub fn is_plugin_enabled(&self, plugin_name: &str) -> bool {
        self.plugin_disabled_reason(plugin_name).is_none()
    }

    /// Returns the resolved `PluginDebugConfig` for a given plugin.
    pub fn get_debug_config(&self, plugin_name: &str) -> PluginDebugConfig {
        self.plugins
            .iter()
            .find(|p| p.name == plugin_name)
            .and_then(|p| p.debug.as_ref())
            .map(|d| d.resolve())
            .unwrap_or_default()
    }

    /// Loads `plugins.toml` from the specified path, or generates a documented default template.
    pub fn load_or_create(config_path: &std::path::Path) -> Self {
        if config_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(config_path) {
                match Self::parse(&content) {
                    Ok(cfg) => return cfg,
                    Err(e) => {
                        log::error!(
                            target: goldsrc_api::consts::log_targets::WASM,
                            "CRITICAL: Failed to parse '{:?}': {e}. Preserving file and using default in-memory config.",
                            config_path
                        );
                        // Backup the corrupted file so administrator edits are not lost
                        let bak_path = config_path.with_extension("toml.bak");
                        let _ = std::fs::copy(config_path, &bak_path);
                        return Self::default();
                    }
                }
            } else {
                log::warn!(
                    target: goldsrc_api::consts::log_targets::WASM,
                    "Failed to read '{:?}', using default discovery.",
                    config_path
                );
                return Self::default();
            }
        }

        let default_template = include_str!("../resources/plugins.template.toml");

        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(config_path, default_template);

        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_plugins_config() {
        let toml_str = r#"
            [[plugins]]
            name = "admin_system"
            enabled = true
            tier = "core"
            debug = true

            [[plugins]]
            name = "vip_core"
            enabled = true
            tier = "gameplay"
            requires = ["admin_system"]
            [plugins.debug]
            level = "trace"
            profile = true
            epoch_limit = 250
            log_file = "vip_debug.log"

            [groups.fun_mods]
            enabled = false
            plugins = ["gungame", "paintball"]

            [[rules]]
            name = "disable_on_warmup"
            when = { cvar = "mp_warmup_time > 0", map = ["fy_*", "aim_*"] }
            action = { pause = ["vip_core"], set_cvar = { "sv_gravity" = 700 } }
        "#;

        let cfg = PluginsConfig::parse(toml_str).unwrap();
        assert_eq!(cfg.plugins.len(), 2);
        assert_eq!(cfg.plugins[0].name, "admin_system");
        assert_eq!(cfg.plugins[0].tier, PluginTier::Core);
        assert!(cfg.plugins[0].requires.is_empty());

        assert_eq!(cfg.plugins[1].name, "vip_core");
        assert_eq!(cfg.plugins[1].tier, PluginTier::Gameplay);
        assert_eq!(cfg.plugins[1].requires, vec!["admin_system"]);

        let debug0 = cfg.get_debug_config("admin_system");
        assert_eq!(debug0.level, PluginLogLevel::Debug);
        assert!(debug0.profile);

        let debug1 = cfg.get_debug_config("vip_core");
        assert_eq!(debug1.level, PluginLogLevel::Trace);
        assert_eq!(debug1.epoch_limit, 250);
        assert_eq!(debug1.log_file, Some("vip_debug.log".to_string()));

        assert!(cfg.is_plugin_enabled("admin_system"));
        assert!(!cfg.is_plugin_enabled("gungame")); // disabled via fun_mods group
        assert!(cfg.is_plugin_enabled("unlisted_plugin"));

        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].name, "disable_on_warmup");
        assert!(cfg.rules[0].when.contains_key("cvar"));
        assert!(cfg.rules[0].action.contains_key("pause"));
    }

    #[test]
    fn test_parse_plugins_config_map_format() {
        let toml_str = r#"
            [plugins.admin_system]
            enabled = true
            tier = "core"
            debug = true

            [plugins.vip_core]
            enabled = false
            tier = "addon"
            requires = "admin_system"

            [groups.fun_mods]
            enabled = false
            plugins = ["gungame", "paintball"]

            [rules.disable_on_dust2]
            when = { map = ["de_dust2"] }
            action = { pause = ["vip_core"] }
        "#;

        let cfg = PluginsConfig::parse(toml_str).unwrap();
        assert_eq!(cfg.plugins.len(), 2);
        let admin = cfg
            .plugins
            .iter()
            .find(|p| p.name == "admin_system")
            .unwrap();
        assert!(admin.enabled);
        assert_eq!(admin.tier, PluginTier::Core);
        assert!(admin.requires.is_empty());

        let vip = cfg.plugins.iter().find(|p| p.name == "vip_core").unwrap();
        assert!(!vip.enabled);
        assert_eq!(vip.tier, PluginTier::Addon);
        assert_eq!(vip.requires, vec!["admin_system"]);

        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].name, "disable_on_dust2");
        assert!(cfg.rules[0].when.contains_key("map"));
        assert!(cfg.rules[0].action.contains_key("pause"));
    }
}
