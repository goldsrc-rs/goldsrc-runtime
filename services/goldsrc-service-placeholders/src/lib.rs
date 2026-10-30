//! Global Placeholder Engine, Registry, and String Interpolator for GoldSrc.rs.

use goldsrc_api::client::Player;
use goldsrc_api::placeholders::{
    PlaceholderCall, PlaceholderHandler, PlaceholderMetadata, parse_placeholder_call,
};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use stitch_rs::define_pipeline;
pub use stitch_rs::flow::FlowControl;

/// Context passing through the placeholder resolution U-cycle.
#[repr(C, align(64))]
pub struct PlaceholderContext<'a> {
    pub ident: &'a str,
    pub caller_idx: i32,
    pub param: &'a str,
    pub resolved: Option<String>,
}

impl<'a> stitch_rs::Blackboard for PlaceholderContext<'a> {}

impl<'a> PlaceholderContext<'a> {
    pub fn new(ident: &'a str, caller_idx: i32, param: &'a str) -> Self {
        Self {
            ident,
            caller_idx,
            param,
            resolved: None,
        }
    }

    pub fn set_resolved(&mut self, val: impl Into<String>) {
        self.resolved = Some(val.into());
    }
}

/// The intent entering placeholder resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceholderIntent<'a> {
    pub ident: &'a str,
}

define_pipeline! {
    name: Placeholder,
    context: PlaceholderContext<'_>,
    intent: PlaceholderIntent<'_>,
    outcome: (),
    error: (),
    vis: pub,
}

static PLACEHOLDER_PIPELINE: LazyLock<RwLock<PlaceholderPipeline>> =
    LazyLock::new(|| RwLock::new(PlaceholderPipeline::new()));

/// Registers a custom middleware layer with the global placeholder pipeline.
pub fn register_placeholder_layer(layer: Arc<dyn PlaceholderLayer>) {
    let mut lock = PLACEHOLDER_PIPELINE
        .write()
        .unwrap_or_else(|e| e.into_inner());
    *lock = lock.clone().with_layer(layer);
}

/// Dispatches a placeholder resolution request through the global SMA U-cycle pipeline.
#[allow(clippy::result_unit_err)]
pub fn dispatch_placeholder_pipeline(
    ctx: &mut PlaceholderContext<'_>,
    intent: &PlaceholderIntent<'_>,
) -> Result<(), ()> {
    let lock = PLACEHOLDER_PIPELINE
        .read()
        .unwrap_or_else(|e| e.into_inner());
    lock.dispatch(ctx, intent)
}

/// Global thread-safe registry of registered placeholder providers.
static PLACEHOLDER_REGISTRY: LazyLock<RwLock<PlaceholderRegistry>> =
    LazyLock::new(|| RwLock::new(PlaceholderRegistry::new()));

/// Pluggable fallback resolver for WASM guest plugins.
pub type PluginPlaceholderFallback = Arc<dyn Fn(&str, i32, &str) -> Option<String> + Send + Sync>;

static PLUGIN_FALLBACK: LazyLock<RwLock<Option<PluginPlaceholderFallback>>> =
    LazyLock::new(|| RwLock::new(None));

/// Configures the global plugin fallback handler for unresolved placeholders.
pub fn set_plugin_placeholder_fallback(f: PluginPlaceholderFallback) {
    let mut lock = match PLUGIN_FALLBACK.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *lock = Some(f);
}

/// Registered entry containing metadata and execution closure.
#[derive(Clone)]
pub struct RegistryEntry {
    pub plugin_name: String,
    pub metadata: PlaceholderMetadata,
    pub handler: Arc<dyn PlaceholderHandler>,
}

/// Errors raised during placeholder lookup and evaluation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaceholderError {
    #[error("placeholder '{{{domain}:{ident}}}' not found in plugin '{domain}'")]
    DomainNotFound { domain: String, ident: String },
    #[error("ambiguous placeholder '{{{ident}}}' registered by multiple plugins: {plugins:?}")]
    Ambiguous { ident: String, plugins: Vec<String> },
    #[error("unknown placeholder '{{{ident}}}'")]
    Unknown { ident: String },
}

/// Dynamic placeholder registry supporting scoped (`plugin:name`) and global (`name`) resolution.
pub struct PlaceholderRegistry {
    entries: HashMap<(String, String), RegistryEntry>, // (plugin_name, placeholder_name) -> Entry
    short_lookup: HashMap<String, Vec<String>>,        // short_name -> list of plugin_names
}

impl Default for PlaceholderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaceholderRegistry {
    /// Creates an empty registry and initializes built-in engine placeholders.
    pub fn new() -> Self {
        let mut reg = Self {
            entries: HashMap::new(),
            short_lookup: HashMap::new(),
        };
        reg.register_builtins();
        reg
    }

    fn register_builtins(&mut self) {
        // 1. {name} / {engine:name(target=...)}
        self.register(
            "engine",
            PlaceholderMetadata {
                name: "name".to_string(),
                description: "Returns player display name".to_string(),
                usage: "{name} or {name(target=1..32)}".to_string(),
                aliases: vec!["player_name".to_string()],
                capability: None,
            },
            Arc::new(|caller: Option<Player>, call: &PlaceholderCall| {
                use goldsrc_api::ClientExt;
                let target_player = resolve_target(caller, call);
                target_player.and_then(|p| p.name()).unwrap_or_else(|| {
                    target_player
                        .map(|p| format!("Player#{}", p.index()))
                        .unwrap_or_else(|| "Server".to_string())
                })
            }),
        );

        // 2. {ip} / {engine:ip}
        self.register(
            "engine",
            PlaceholderMetadata {
                name: "ip".to_string(),
                description: "Returns player IP address".to_string(),
                usage: "{ip} or {ip(target=1..32)}".to_string(),
                aliases: vec!["player_ip".to_string()],
                capability: None,
            },
            Arc::new(|caller: Option<Player>, call: &PlaceholderCall| {
                use goldsrc_api::ClientExt;
                let target_player = resolve_target(caller, call);
                target_player
                    .map(|p| p.ip())
                    .unwrap_or_else(|| "127.0.0.1".to_string())
            }),
        );

        // 3. {steamid} / {authid}
        self.register(
            "engine",
            PlaceholderMetadata {
                name: "authid".to_string(),
                description: "Returns player Steam ID / Auth ID".to_string(),
                usage: "{authid} or {steamid}".to_string(),
                aliases: vec!["id".to_string(), "steamid".to_string()],
                capability: None,
            },
            Arc::new(|caller: Option<Player>, call: &PlaceholderCall| {
                use goldsrc_api::ClientExt;
                let target_player = resolve_target(caller, call);
                target_player
                    .map(|p| p.auth_id())
                    .unwrap_or_else(|| "STEAM_ID_LAN".to_string())
            }),
        );

        // 3b. {userid} / {engine:userid}
        self.register(
            "engine",
            PlaceholderMetadata {
                name: "userid".to_string(),
                description: "Returns player server unique user ID".to_string(),
                usage: "{userid} or {userid(target=1..32)}".to_string(),
                aliases: vec!["user_id".to_string()],
                capability: None,
            },
            Arc::new(|caller: Option<Player>, call: &PlaceholderCall| {
                use goldsrc_api::ClientExt;
                let target_player = resolve_target(caller, call);
                target_player
                    .map(|p| p.user_id().to_string())
                    .unwrap_or_else(|| "0".to_string())
            }),
        );

        // 4. {health} / {hp}
        self.register(
            "engine",
            PlaceholderMetadata {
                name: "health".to_string(),
                description: "Returns player current health points".to_string(),
                usage: "{health} or {hp}".to_string(),
                aliases: vec!["hp".to_string()],
                capability: None,
            },
            Arc::new(|caller: Option<Player>, call: &PlaceholderCall| {
                use goldsrc_api::EntityExt;
                let target_player = resolve_target(caller, call);
                target_player
                    .map(|p| (p.health().current() as i32).to_string())
                    .unwrap_or_else(|| "100".to_string())
            }),
        );

        // 5. {armor} / {ap}
        self.register(
            "engine",
            PlaceholderMetadata {
                name: "armor".to_string(),
                description: "Returns player current armor points".to_string(),
                usage: "{armor} or {ap}".to_string(),
                aliases: vec!["ap".to_string()],
                capability: None,
            },
            Arc::new(|caller: Option<Player>, call: &PlaceholderCall| {
                use goldsrc_api::PlayerExt;
                let target_player = resolve_target(caller, call);
                target_player
                    .map(|p| (p.armorvalue() as i32).to_string())
                    .unwrap_or_else(|| "0".to_string())
            }),
        );

        // 6. {log:date-time} / {log:timestamp} / {time}
        self.register(
            "log",
            PlaceholderMetadata {
                name: "date-time".to_string(),
                description: "Returns current formatted timestamp".to_string(),
                usage: "{log:date-time} or {log:timestamp}".to_string(),
                aliases: vec!["timestamp".to_string(), "time".to_string()],
                capability: None,
            },
            Arc::new(|_caller: Option<Player>, _call: &PlaceholderCall| {
                let now = std::time::SystemTime::now();
                let duration = now
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let total_secs = duration.as_secs();
                let sec = total_secs % 60;
                let min = (total_secs / 60) % 60;
                let hour = (total_secs / 3600) % 24;
                format!("{:02}:{:02}:{:02}", hour, min, sec)
            }),
        );

        // 7. {log:date}
        self.register(
            "log",
            PlaceholderMetadata {
                name: "date".to_string(),
                description: "Returns current date".to_string(),
                usage: "{log:date} or {date}".to_string(),
                aliases: vec!["date".to_string()],
                capability: None,
            },
            Arc::new(|_caller: Option<Player>, _call: &PlaceholderCall| {
                let now = std::time::SystemTime::now();
                let duration = now
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                let days = duration.as_secs() / 86400;
                // Approximate epoch date for general string interpolation
                format!("epoch+{}d", days)
            }),
        );
    }

    /// Registers a new placeholder provider in the registry.
    pub fn register(
        &mut self,
        plugin_name: &str,
        metadata: PlaceholderMetadata,
        handler: Arc<dyn PlaceholderHandler>,
    ) {
        let name_lower = metadata.name.to_lowercase();
        let plugin_lower = plugin_name.to_lowercase();

        self.short_lookup
            .entry(name_lower.clone())
            .or_default()
            .push(plugin_lower.clone());

        for alias in &metadata.aliases {
            self.short_lookup
                .entry(alias.to_lowercase())
                .or_default()
                .push(plugin_lower.clone());
        }

        self.entries.insert(
            (plugin_lower, name_lower),
            RegistryEntry {
                plugin_name: plugin_name.to_string(),
                metadata,
                handler,
            },
        );
    }

    /// Resolves and formats a placeholder call string.
    pub fn evaluate_call(
        &self,
        caller: Option<Player>,
        call: &PlaceholderCall,
    ) -> Result<String, PlaceholderError> {
        let ident_lower = call.ident.to_lowercase();

        // 1. If explicit domain is given (e.g. {stats:rank})
        if let Some(ref domain) = call.domain {
            let domain_lower = domain.to_lowercase();
            if let Some(entry) = self
                .entries
                .get(&(domain_lower.clone(), ident_lower.clone()))
            {
                return Ok(entry.handler.evaluate(caller, call));
            }
            return Err(PlaceholderError::DomainNotFound {
                domain: domain.clone(),
                ident: call.ident.clone(),
            });
        }

        // 2. Short name / alias lookup (e.g. {rank} or {hp})
        if let Some(plugins) = self.short_lookup.get(&ident_lower) {
            if plugins.len() == 1 {
                let plugin = &plugins[0];
                // Try exact name or search among registered aliases for this plugin
                if let Some(entry) = self.entries.get(&(plugin.clone(), ident_lower.clone())) {
                    return Ok(entry.handler.evaluate(caller, call));
                }
                for entry in self.entries.values() {
                    if entry.plugin_name.eq_ignore_ascii_case(plugin)
                        && (entry.metadata.name.eq_ignore_ascii_case(&ident_lower)
                            || entry
                                .metadata
                                .aliases
                                .iter()
                                .any(|a| a.eq_ignore_ascii_case(&ident_lower)))
                    {
                        return Ok(entry.handler.evaluate(caller, call));
                    }
                }
            } else if plugins.len() > 1 {
                return Err(PlaceholderError::Ambiguous {
                    ident: call.ident.clone(),
                    plugins: plugins.clone(),
                });
            }
        }

        Err(PlaceholderError::Unknown {
            ident: call.ident.clone(),
        })
    }
}

fn resolve_target(caller: Option<Player>, call: &PlaceholderCall) -> Option<Player> {
    if let Some(target_str) = call.get_param("target", 0)
        && let Ok(slot) = target_str.parse::<i32>()
        && goldsrc_api::PlayerSlot::VALID_RANGE.contains(&slot)
    {
        return Some(Player::new(slot));
    }
    caller
}

/// Registers a custom placeholder provider with default metadata.
pub fn register_placeholder<F>(name: &str, description: &str, handler: F)
where
    F: Fn(Option<Player>, &PlaceholderCall) -> String + Send + Sync + 'static,
{
    let mut reg = match PLACEHOLDER_REGISTRY.write() {
        Ok(r) => r,
        Err(e) => e.into_inner(),
    };
    reg.register(
        "custom",
        PlaceholderMetadata {
            name: name.to_string(),
            description: description.to_string(),
            usage: format!("{{{name}}}"),
            aliases: Vec::new(),
            capability: None,
        },
        Arc::new(handler),
    );
}

/// Registers a custom placeholder provider with full metadata.
pub fn register_placeholder_with_metadata<F>(
    plugin_name: &str,
    metadata: PlaceholderMetadata,
    handler: F,
) where
    F: Fn(Option<Player>, &PlaceholderCall) -> String + Send + Sync + 'static,
{
    let mut reg = match PLACEHOLDER_REGISTRY.write() {
        Ok(r) => r,
        Err(e) => e.into_inner(),
    };
    reg.register(plugin_name, metadata, Arc::new(handler));
}

/// Dispatches a placeholder resolution request inside a WASM plugin.
pub fn dispatch_local_placeholder(name: &str, caller_idx: i32, param: &str) -> Option<String> {
    let caller = if goldsrc_api::PlayerSlot::VALID_RANGE.contains(&caller_idx) {
        Some(Player::new(caller_idx))
    } else {
        None
    };
    let raw_expr = if param.is_empty() {
        name.to_string()
    } else {
        format!("{name}({param})")
    };
    let call = parse_placeholder_call(&raw_expr).ok()?;
    let reg = match PLACEHOLDER_REGISTRY.read() {
        Ok(r) => r,
        Err(e) => e.into_inner(),
    };
    reg.evaluate_call(caller, &call).ok()
}

/// Replaces all `{...}` placeholders with an optional dispatcher callback.
pub fn format_placeholders_with_dispatcher<F>(
    template: &str,
    caller: impl Into<Option<Player>>,
    mut fallback_dispatcher: Option<F>,
) -> String
where
    F: FnMut(&str, i32, &str) -> Option<String>,
{
    let caller = caller.into();
    let caller_index = caller.map(|p| p.index()).unwrap_or(0);
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek()
                && (next == '{' || next == '}' || next == '\\')
            {
                out.push(next);
                chars.next();
                continue;
            }
            out.push('\\');
        } else if c == '{' {
            let mut inner = String::new();
            let mut closed = false;
            for ic in chars.by_ref() {
                if ic == '}' {
                    closed = true;
                    break;
                }
                inner.push(ic);
            }

            if !closed {
                out.push('{');
                out.push_str(&inner);
                continue;
            }

            let reg = match PLACEHOLDER_REGISTRY.read() {
                Ok(r) => r,
                Err(e) => e.into_inner(),
            };

            match parse_placeholder_call(&inner) {
                Ok(call) => match reg.evaluate_call(caller, &call) {
                    Ok(val) => out.push_str(&val),
                    Err(_) => {
                        let param = call
                            .get_positional(0)
                            .or_else(|| call.get_named("target"))
                            .unwrap_or_default();

                        let mut ctx = PlaceholderContext::new(&call.ident, caller_index, param);
                        let intent = PlaceholderIntent { ident: &call.ident };

                        let resolved = if dispatch_placeholder_pipeline(&mut ctx, &intent).is_ok()
                            && ctx.resolved.is_some()
                        {
                            ctx.resolved
                        } else if let Some(ref mut d) = fallback_dispatcher {
                            d(&call.ident, caller_index, param)
                        } else {
                            let global_cb = {
                                let lock = match PLUGIN_FALLBACK.read() {
                                    Ok(guard) => guard,
                                    Err(poisoned) => poisoned.into_inner(),
                                };
                                lock.clone()
                            };
                            global_cb.and_then(|cb| cb(&call.ident, caller_index, param))
                        };

                        if let Some(val) = resolved {
                            out.push_str(&val);
                        } else if let Some(ref def) = call.default {
                            out.push_str(def);
                        } else {
                            out.push('{');
                            out.push_str(&inner);
                            out.push('}');
                        }
                    }
                },
                Err(_) => {
                    out.push('{');
                    out.push_str(&inner);
                    out.push('}');
                }
            }
        } else {
            out.push(c);
        }
    }

    out
}

/// Replaces all `{...}` placeholders in `template` evaluated in the context of `caller`.
pub fn format_placeholders(template: &str, caller: impl Into<Option<Player>>) -> String {
    format_placeholders_with_dispatcher::<fn(&str, i32, &str) -> Option<String>>(
        template, caller, None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_placeholders() {
        let player = Player::new(1);
        let formatted = format_placeholders("Hello, {name}! Your IP is {ip}, HP: {hp}", player);
        assert!(formatted.contains("Hello, Player#1!"));
        assert!(formatted.contains("127.0.0.1"));
        assert!(formatted.contains("HP: 0")); // default unspawned player health in pure unit test
    }

    #[test]
    fn test_placeholder_default_fallback_and_escapes() {
        let player = Player::new(1);
        let formatted = format_placeholders(
            "Hello, {unknown_var='Guest'}! Escaped: \\{escaped_tag\\}",
            player,
        );
        assert_eq!(formatted, "Hello, Guest! Escaped: {escaped_tag}");
    }

    #[test]
    fn test_placeholder_context_alignment() {
        let ctx = PlaceholderContext::new("test", 1, "arg");
        assert_eq!(std::mem::align_of_val(&ctx), 64);
        assert!(ctx.resolved.is_none());
    }

    struct MockPlaceholderLayer;

    impl PlaceholderLayer for MockPlaceholderLayer {
        fn on_enter(
            &self,
            ctx: &mut PlaceholderContext<'_>,
            intent: &PlaceholderIntent<'_>,
        ) -> FlowControl<(), (), ()> {
            if intent.ident == "sma_tag" {
                ctx.set_resolved("SMA_RESOLVED_VALUE");
                FlowControl::ShortCircuit(())
            } else {
                FlowControl::Proceed(())
            }
        }

        fn on_exit(
            &self,
            _ctx: &mut PlaceholderContext<'_>,
            _intent: &PlaceholderIntent<'_>,
            _outcome: &mut Result<(), ()>,
        ) {
        }
    }

    #[test]
    fn test_placeholder_pipeline_u_cycle() {
        let layer = Arc::new(MockPlaceholderLayer);
        let pipeline = PlaceholderPipeline::new().with_layer(layer);

        let mut ctx = PlaceholderContext::new("sma_tag", 1, "");
        let intent = PlaceholderIntent { ident: "sma_tag" };

        let res = pipeline.dispatch(&mut ctx, &intent);
        assert_eq!(res, Ok(()));
        assert_eq!(ctx.resolved.as_deref(), Some("SMA_RESOLVED_VALUE"));
    }
}
