//! Native moderation subsystems and default command executors (kick, mute, ban) for GoldSrc.rs.
//!
//! Provides the fallback execution layer for server moderation commands in accordance
//! with the 3-tier command routing architecture. External plugins can override these
//! executors using `goldsrc_api::command::override_command_executor`.

use goldsrc_api::HUD_PRINTCHAT;
use goldsrc_api::command::Command;
use goldsrc_spi::engine::Engine;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use stitch_rs::define_pipeline;
pub use stitch_rs::flow::FlowControl;

/// Type of moderation action executed in the U-cycle pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModerationIntentKind {
    Kick,
    Mute { duration: Option<Duration> },
    Unmute,
    Ban { duration: Option<Duration> },
    Unban,
}

/// The intent entering the moderation U-cycle pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationIntent<'a> {
    pub kind: ModerationIntentKind,
    pub target: &'a str,
    pub reason: &'a str,
    pub caller: i32,
}

/// 64-byte aligned execution context for moderation operations conforming to SMA.
#[repr(C, align(64))]
pub struct ModerationContext<'a> {
    pub target_slot: Option<i32>,
    pub target_name: &'a str,
    pub target_identity: &'a str,
    pub caller: i32,
    pub is_suppressed: bool,
    pub suppression_reason: Option<String>,
}

impl<'a> stitch_rs::Blackboard for ModerationContext<'a> {}

impl<'a> ModerationContext<'a> {
    pub fn new(
        target_slot: Option<i32>,
        target_name: &'a str,
        target_identity: &'a str,
        caller: i32,
    ) -> Self {
        Self {
            target_slot,
            target_name,
            target_identity,
            caller,
            is_suppressed: false,
            suppression_reason: None,
        }
    }

    pub fn suppress(&mut self, reason: impl Into<String>) {
        self.is_suppressed = true;
        self.suppression_reason = Some(reason.into());
    }
}

define_pipeline! {
    name: Moderation,
    context: ModerationContext<'_>,
    intent: ModerationIntent<'_>,
    outcome: (),
    error: (),
    vis: pub,
}

static MODERATION_PIPELINE: LazyLock<RwLock<ModerationPipeline>> =
    LazyLock::new(|| RwLock::new(ModerationPipeline::new()));

/// Registers a custom middleware layer with the global moderation pipeline.
pub fn register_moderation_layer(layer: Arc<dyn ModerationLayer>) {
    let mut lock = MODERATION_PIPELINE
        .write()
        .unwrap_or_else(|e| e.into_inner());
    *lock = lock.clone().with_layer(layer);
}

/// Dispatches a moderation action through the global SMA U-cycle pipeline.
#[allow(clippy::result_unit_err)]
pub fn dispatch_moderation(
    ctx: &mut ModerationContext<'_>,
    intent: &ModerationIntent<'_>,
) -> Result<(), ()> {
    let pipeline = {
        let lock = MODERATION_PIPELINE
            .read()
            .unwrap_or_else(|e| e.into_inner());
        lock.clone()
    };
    pipeline.dispatch(ctx, intent)
}

/// Provider hook for engine callbacks (e.g. HostRuntime::engine).
pub type EngineProvider = Arc<dyn Fn() -> Option<Arc<dyn Engine>> + Send + Sync>;

static ENGINE_PROVIDER: LazyLock<RwLock<Option<EngineProvider>>> =
    LazyLock::new(|| RwLock::new(None));

/// Configures the engine provider callback.
pub fn set_engine_provider<F>(f: F)
where
    F: Fn() -> Option<Arc<dyn Engine>> + Send + Sync + 'static,
{
    let mut lock = ENGINE_PROVIDER.write().unwrap_or_else(|e| e.into_inner());
    *lock = Some(Arc::new(f));
}

/// Retrieves the active game engine bridge if available.
pub fn active_engine() -> Option<Arc<dyn Engine>> {
    let lock = ENGINE_PROVIDER.read().unwrap_or_else(|e| e.into_inner());
    lock.as_ref().and_then(|f| f())
}

/// Entry describing an active or expired ban.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BanEntry {
    /// Target identity: SteamID (e.g. `STEAM_0:1:23456`) or IP address.
    pub identity: String,
    /// Reason for the ban.
    pub reason: String,
    /// Moderator or source issuing the ban.
    pub admin: String,
    /// Unix timestamp (seconds) when the ban was issued.
    pub created_at: u64,
    /// Optional Unix timestamp (seconds) when the ban expires (`None` = permanent).
    pub expires_at: Option<u64>,
}

impl BanEntry {
    /// Returns `true` if the ban has expired relative to current system time.
    pub fn is_expired(&self) -> bool {
        if let Some(exp) = self.expires_at {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            now >= exp
        } else {
            false
        }
    }
}

/// In-memory ban registry.
#[derive(Default)]
pub struct BanRegistry {
    bans: HashMap<String, BanEntry>,
}

impl BanRegistry {
    /// Creates a new empty ban registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a ban entry for `identity` (case-insensitive keying).
    pub fn ban(
        &mut self,
        identity: &str,
        duration: Option<Duration>,
        reason: &str,
        admin: &str,
    ) -> BanEntry {
        let key = identity.trim().to_ascii_lowercase();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let expires_at = duration.map(|d| now + d.as_secs());
        let entry = BanEntry {
            identity: identity.trim().to_string(),
            reason: reason.trim().to_string(),
            admin: admin.trim().to_string(),
            created_at: now,
            expires_at,
        };

        self.bans.insert(key, entry.clone());
        entry
    }

    /// Removes a ban entry for `identity`.
    pub fn unban(&mut self, identity: &str) -> bool {
        let key = identity.trim().to_ascii_lowercase();
        self.bans.remove(&key).is_some()
    }

    /// Returns the active ban for `identity` if present and unexpired.
    pub fn check(&self, identity: &str) -> Option<&BanEntry> {
        let key = identity.trim().to_ascii_lowercase();
        if let Some(entry) = self.bans.get(&key) {
            if entry.is_expired() {
                None
            } else {
                Some(entry)
            }
        } else {
            None
        }
    }

    /// Prunes expired bans and returns all remaining active ban entries.
    pub fn list(&mut self) -> Vec<BanEntry> {
        self.bans.retain(|_, b| !b.is_expired());
        self.bans.values().cloned().collect()
    }

    /// Clears all ban records.
    pub fn clear(&mut self) {
        self.bans.clear();
    }
}

/// Record of an active player mute.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MuteRecord {
    pub expires_at: Option<Instant>,
    pub reason: String,
    pub slot: i32,
}

/// In-memory player mute registry.
#[derive(Default)]
pub struct MuteRegistry {
    mutes: HashMap<i32, MuteRecord>,
}

impl MuteRegistry {
    /// Creates a new empty mute registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mutes a player slot for an optional duration.
    pub fn mute(&mut self, slot: i32, duration: Option<Duration>, reason: &str) {
        let expires_at = duration.map(|d| Instant::now() + d);
        self.mutes.insert(
            slot,
            MuteRecord {
                slot,
                expires_at,
                reason: reason.to_string(),
            },
        );
    }

    /// Unmutes a player slot.
    pub fn unmute(&mut self, slot: i32) -> bool {
        self.mutes.remove(&slot).is_some()
    }

    /// Checks if a player slot is currently muted, auto-evicting expired mutes.
    pub fn is_muted(&mut self, slot: i32) -> bool {
        if let Some(record) = self.mutes.get(&slot) {
            if let Some(exp) = record.expires_at
                && Instant::now() >= exp
            {
                self.mutes.remove(&slot);
                return false;
            }
            true
        } else {
            false
        }
    }

    /// Clears all mutes.
    pub fn clear(&mut self) {
        self.mutes.clear();
    }
}

static BAN_REGISTRY: LazyLock<RwLock<BanRegistry>> =
    LazyLock::new(|| RwLock::new(BanRegistry::default()));

static MUTE_REGISTRY: LazyLock<RwLock<MuteRegistry>> =
    LazyLock::new(|| RwLock::new(MuteRegistry::default()));

/// Checks if an identity (SteamID or IP) is banned in the global registry.
pub fn is_identity_banned(identity: &str) -> Option<BanEntry> {
    BAN_REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .check(identity)
        .cloned()
}

/// Checks if a player slot is currently muted in the global registry.
pub fn is_player_muted(slot: i32) -> bool {
    MUTE_REGISTRY
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .is_muted(slot)
}

/// Parses human duration strings like `10s`, `5m`, `2h`, `1d`, `0`, or bare seconds.
/// Returns `None` for permanent duration (`0`, `perm`, `permanent`).
pub fn parse_duration(s: &str) -> Result<Option<Duration>, &'static str> {
    let s = s.trim().to_ascii_lowercase();
    if s == "0" || s == "perm" || s == "permanent" {
        return Ok(None);
    }

    let (num_str, multiplier) = if let Some(stripped) = s.strip_suffix('s') {
        (stripped, 1)
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, 60)
    } else if let Some(stripped) = s.strip_suffix('h') {
        (stripped, 3600)
    } else if let Some(stripped) = s.strip_suffix('d') {
        (stripped, 86400)
    } else if let Some(stripped) = s.strip_suffix('w') {
        (stripped, 604800)
    } else {
        (s.as_str(), 60) // Bare number defaults to minutes in GoldSrc moderation convention
    };

    let count: u64 = num_str
        .parse()
        .map_err(|_| "Invalid numeric duration format")?;
    if count == 0 {
        return Ok(None);
    }
    let secs = count
        .checked_mul(multiplier)
        .ok_or("Duration calculation overflow")?;
    Ok(Some(Duration::from_secs(secs)))
}

/// Drops / kicks a connected player using the active game engine backend.
pub fn drop_client(slot: i32, reason: &str) {
    if let Some(engine) = active_engine() {
        let user_id = engine.player_user_id(slot);
        let kick_cmd = if user_id > 0 {
            format!("kick #{user_id} {reason}\n")
        } else {
            let name = engine
                .player_name(slot)
                .unwrap_or_else(|| format!("player_{slot}"));
            format!("kick \"{name}\" {reason}\n")
        };
        engine.server_command(&kick_cmd);
        log::info!(
            target: "goldsrc::moderation",
            "[Moderation] Dropped client slot {} (user_id #{}, reason: '{}')",
            slot, user_id, reason
        );
    }
}

/// Resolves a player target by slot index, user ID (`#123`), or matching name (exact first, prefix next, then substring).
pub fn resolve_player_target(target: &str) -> Option<(i32, String, String)> {
    let engine = active_engine()?;
    let trimmed = target.trim();

    // 1. User ID check: #123
    if let Some(uid_str) = trimmed.strip_prefix('#')
        && let Ok(uid) = uid_str.parse::<u32>()
    {
        for slot in goldsrc_api::PlayerSlot::all().map(|s| s.index()) {
            if engine.entity_is_valid(slot) && engine.player_user_id(slot) == uid {
                let name = engine.player_name(slot).unwrap_or_default();
                let auth = engine.player_auth_id(slot).unwrap_or_default();
                return Some((slot, name, auth));
            }
        }
    }

    // 2. Direct slot index: PlayerSlot::VALID_RANGE
    if let Ok(slot) = trimmed.parse::<i32>()
        && goldsrc_api::PlayerSlot::VALID_RANGE.contains(&slot)
        && engine.entity_is_valid(slot)
    {
        let name = engine.player_name(slot).unwrap_or_default();
        let auth = engine.player_auth_id(slot).unwrap_or_default();
        return Some((slot, name, auth));
    }

    let query = trimmed.to_ascii_lowercase();

    // 3a. Exact name match first (case-insensitive) - prevents hijacking e.g. "Admin" vs "AdminFake"
    for slot in goldsrc_api::PlayerSlot::all().map(|s| s.index()) {
        if engine.entity_is_valid(slot)
            && let Some(name) = engine.player_name(slot)
            && name.to_ascii_lowercase() == query
        {
            let auth = engine.player_auth_id(slot).unwrap_or_default();
            return Some((slot, name, auth));
        }
    }

    // 3b. Name prefix match
    for slot in goldsrc_api::PlayerSlot::all().map(|s| s.index()) {
        if engine.entity_is_valid(slot)
            && let Some(name) = engine.player_name(slot)
            && name.to_ascii_lowercase().starts_with(&query)
        {
            let auth = engine.player_auth_id(slot).unwrap_or_default();
            return Some((slot, name, auth));
        }
    }

    // 3c. Name substring match fallback
    for slot in goldsrc_api::PlayerSlot::all().map(|s| s.index()) {
        if engine.entity_is_valid(slot)
            && let Some(name) = engine.player_name(slot)
            && name.to_ascii_lowercase().contains(&query)
        {
            let auth = engine.player_auth_id(slot).unwrap_or_default();
            return Some((slot, name, auth));
        }
    }

    None
}

/// Registers native moderation commands (`kick`, `mute`, `unmute`, `ban`, `unban`, `banlist`)
/// with the global command registry as Tier 2 default fallback executors.
pub fn register_moderation_commands() {
    // 1. `kick <target> [reason...]`
    goldsrc_api::command::register_command(
        Command::builder("kick")
            .description("Kicks a player from the server")
            .usage("kick <#userid|name|slot> [reason]")
            .build(),
        |caller, raw_args| {
            let args: Vec<&str> = raw_args.split_whitespace().collect();
            if args.is_empty() {
                log::warn!("[Moderation] Usage: kick <#userid|name|slot> [reason]");
                return false;
            }

            let target_str = args[0];
            let reason = if args.len() > 1 {
                args[1..].join(" ")
            } else {
                "Kicked by moderator".to_string()
            };

            if let Some((slot, name, auth)) = resolve_player_target(target_str) {
                let mut ctx = ModerationContext::new(Some(slot), &name, &auth, caller);
                let intent = ModerationIntent {
                    kind: ModerationIntentKind::Kick,
                    target: target_str,
                    reason: &reason,
                    caller,
                };

                if dispatch_moderation(&mut ctx, &intent).is_err() {
                    let msg = ctx
                        .suppression_reason
                        .as_deref()
                        .unwrap_or("Suppressed by policy");
                    log::warn!("[Moderation] Kick blocked: {}", msg);
                    return false;
                }

                drop_client(slot, &reason);
                if caller == 0 {
                    println!(
                        "[Moderation] Player '{}' (slot {}) was kicked: {}",
                        name, slot, reason
                    );
                }
                true
            } else {
                log::warn!("[Moderation] Player target '{}' not found.", target_str);
                false
            }
        },
    );

    // 2. `mute <target> [duration] [reason...]`
    goldsrc_api::command::register_command(
        Command::builder("mute")
            .description("Mutes a player from text chat")
            .usage("mute <#userid|name|slot> [duration] [reason]")
            .build(),
        |caller, raw_args| {
            let args: Vec<&str> = raw_args.split_whitespace().collect();
            if args.is_empty() {
                log::warn!("[Moderation] Usage: mute <#userid|name|slot> [duration] [reason]");
                return false;
            }

            let target_str = args[0];
            let (duration, reason) = if args.len() > 1 {
                match parse_duration(args[1]) {
                    Ok(dur) => {
                        let r = if args.len() > 2 {
                            args[2..].join(" ")
                        } else {
                            "Chat violation".to_string()
                        };
                        (dur, r)
                    }
                    Err(_) => {
                        // Duration not provided, treated as reason
                        (Some(Duration::from_secs(300)), args[1..].join(" "))
                    }
                }
            } else {
                (Some(Duration::from_secs(300)), "Chat violation".to_string())
            };

            if let Some((slot, name, auth)) = resolve_player_target(target_str) {
                let mut ctx = ModerationContext::new(Some(slot), &name, &auth, caller);
                let intent = ModerationIntent {
                    kind: ModerationIntentKind::Mute { duration },
                    target: target_str,
                    reason: &reason,
                    caller,
                };

                if dispatch_moderation(&mut ctx, &intent).is_err() {
                    let msg = ctx
                        .suppression_reason
                        .as_deref()
                        .unwrap_or("Suppressed by policy");
                    log::warn!("[Moderation] Mute blocked: {}", msg);
                    return false;
                }

                MUTE_REGISTRY
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .mute(slot, duration, &reason);

                if let Some(engine) = active_engine() {
                    let dur_msg = duration
                        .map(|d| format!("for {}s", d.as_secs()))
                        .unwrap_or_else(|| "permanently".to_string());
                    engine.client_print(
                        slot,
                        HUD_PRINTCHAT,
                        &format!(
                            "[Moderation] You have been muted {} (reason: {}).\n",
                            dur_msg, reason
                        ),
                    );
                }

                if caller == 0 {
                    println!("[Moderation] Player '{}' (slot {}) muted.", name, slot);
                }
                true
            } else {
                log::warn!("[Moderation] Target player '{}' not found.", target_str);
                false
            }
        },
    );

    // 3. `unmute <target>`
    goldsrc_api::command::register_command(
        Command::builder("unmute")
            .description("Unmutes a muted player")
            .usage("unmute <#userid|name|slot>")
            .build(),
        |caller, raw_args| {
            let target_str = raw_args.trim();
            if target_str.is_empty() {
                log::warn!("[Moderation] Usage: unmute <#userid|name|slot>");
                return false;
            }

            if let Some((slot, name, _)) = resolve_player_target(target_str) {
                let unmuted = MUTE_REGISTRY
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .unmute(slot);

                if unmuted {
                    if let Some(engine) = active_engine() {
                        engine.client_print(
                            slot,
                            HUD_PRINTCHAT,
                            "[Moderation] You have been unmuted. You can chat now.\n",
                        );
                    }
                    if caller == 0 {
                        println!(
                            "[Moderation] Player '{}' (slot {}) was unmuted.",
                            name, slot
                        );
                    }
                    true
                } else {
                    log::warn!("[Moderation] Player '{}' was not muted.", name);
                    false
                }
            } else {
                log::warn!("[Moderation] Target player '{}' not found.", target_str);
                false
            }
        },
    );

    // 4. `ban <target> <duration> [reason...]`
    goldsrc_api::command::register_command(
        Command::builder("ban")
            .description("Bans an identity or connected player")
            .usage("ban <target|steamid|ip> <duration> [reason]")
            .build(),
        |caller, raw_args| {
            let args: Vec<&str> = raw_args.split_whitespace().collect();
            if args.len() < 2 {
                log::warn!("[Moderation] Usage: ban <target|steamid|ip> <duration> [reason]");
                return false;
            }

            let target_str = args[0];
            let duration = match parse_duration(args[1]) {
                Ok(d) => d,
                Err(err) => {
                    log::warn!("[Moderation] {}: {}", err, args[1]);
                    return false;
                }
            };
            let reason = if args.len() > 2 {
                args[2..].join(" ")
            } else {
                "Banned by administrator".to_string()
            };

            let admin_str = if caller > 0 {
                format!("Admin#{}", caller)
            } else {
                "ServerConsole".to_string()
            };

            // Check if online player matches
            if let Some((slot, name, auth)) = resolve_player_target(target_str) {
                let ban_target =
                    if !auth.is_empty() && auth != "STEAM_ID_LAN" && auth != "STEAM_ID_PENDING" {
                        auth
                    } else {
                        name.clone()
                    };

                let mut ctx = ModerationContext::new(Some(slot), &name, &ban_target, caller);
                let intent = ModerationIntent {
                    kind: ModerationIntentKind::Ban { duration },
                    target: target_str,
                    reason: &reason,
                    caller,
                };

                if dispatch_moderation(&mut ctx, &intent).is_err() {
                    let msg = ctx
                        .suppression_reason
                        .as_deref()
                        .unwrap_or("Suppressed by policy");
                    log::warn!("[Moderation] Ban blocked: {}", msg);
                    return false;
                }

                let entry = BAN_REGISTRY.write().unwrap_or_else(|e| e.into_inner()).ban(
                    &ban_target,
                    duration,
                    &reason,
                    &admin_str,
                );

                drop_client(slot, &reason);

                if caller == 0 {
                    println!(
                        "[Moderation] Banned online player '{}' ({}) - reason: '{}'",
                        name, entry.identity, entry.reason
                    );
                }
                true
            } else {
                // Offline direct identity ban
                let mut ctx = ModerationContext::new(None, target_str, target_str, caller);
                let intent = ModerationIntent {
                    kind: ModerationIntentKind::Ban { duration },
                    target: target_str,
                    reason: &reason,
                    caller,
                };

                if dispatch_moderation(&mut ctx, &intent).is_err() {
                    let msg = ctx
                        .suppression_reason
                        .as_deref()
                        .unwrap_or("Suppressed by policy");
                    log::warn!("[Moderation] Ban blocked: {}", msg);
                    return false;
                }

                let entry = BAN_REGISTRY
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .ban(target_str, duration, &reason, &admin_str);

                if caller == 0 {
                    println!(
                        "[Moderation] Banned identity '{}' - reason: '{}'",
                        entry.identity, entry.reason
                    );
                }
                true
            }
        },
    );

    // 5. `unban <identity>`
    goldsrc_api::command::register_command(
        Command::builder("unban")
            .description("Unbans a previously banned identity")
            .usage("unban <steamid|ip|name>")
            .build(),
        |caller, raw_args| {
            let identity = raw_args.trim();
            if identity.is_empty() {
                log::warn!("[Moderation] Usage: unban <steamid|ip|name>");
                return false;
            }

            let unbanned = BAN_REGISTRY
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .unban(identity);

            if unbanned {
                if caller == 0 {
                    println!(
                        "[Moderation] Successfully unbanned identity '{}'.",
                        identity
                    );
                }
                true
            } else {
                log::warn!(
                    "[Moderation] Identity '{}' was not found in ban registry.",
                    identity
                );
                false
            }
        },
    );

    // 6. `banlist`
    goldsrc_api::command::register_command(
        Command::builder("banlist")
            .description("Lists all active bans")
            .usage("banlist")
            .build(),
        |_caller, _args| {
            let mut reg = BAN_REGISTRY.write().unwrap_or_else(|e| e.into_inner());
            let list = reg.list();
            println!("--- Active Server Bans ({}) ---", list.len());
            for b in list {
                let exp_str = b
                    .expires_at
                    .map(|exp| {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        if exp > now {
                            format!("{}s left", exp - now)
                        } else {
                            "expired".to_string()
                        }
                    })
                    .unwrap_or_else(|| "permanent".to_string());
                println!(
                    "  * {:<24} [{}] reason: '{}' (by {})",
                    b.identity, exp_str, b.reason, b.admin
                );
            }
            true
        },
    );

    // Register SMA chat layer for mute enforcement
    goldsrc_service_chat::register_chat_layer(std::sync::Arc::new(MuteChatLayer));
}

/// Chat layer interceptor enforcing player moderation mute state via SMA U-cycle.
pub struct MuteChatLayer;

impl goldsrc_service_chat::ChatLayer for MuteChatLayer {
    fn on_enter(
        &self,
        msg: &mut goldsrc_api::chat::ChatMessage,
    ) -> stitch_rs::flow::FlowControl<(), (), ()> {
        if is_player_muted(msg.sender.index()) {
            if let Some(engine) = active_engine() {
                engine.client_print(
                    msg.sender.index(),
                    HUD_PRINTCHAT,
                    "[Moderation] You are muted and cannot send chat messages.\n",
                );
            }
            msg.is_blocked = true;
            stitch_rs::flow::FlowControl::Halt(())
        } else {
            stitch_rs::flow::FlowControl::Proceed(())
        }
    }

    fn on_exit(&self, _msg: &mut goldsrc_api::chat::ChatMessage, _outcome: &mut Result<(), ()>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_units() {
        assert_eq!(parse_duration("0").unwrap(), None);
        assert_eq!(parse_duration("perm").unwrap(), None);
        assert_eq!(parse_duration("0s").unwrap(), None);
        assert_eq!(parse_duration("0m").unwrap(), None);
        assert_eq!(
            parse_duration("30s").unwrap(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_duration("5m").unwrap(),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            parse_duration("2h").unwrap(),
            Some(Duration::from_secs(7200))
        );
        assert_eq!(
            parse_duration("1d").unwrap(),
            Some(Duration::from_secs(86400))
        );
        assert_eq!(
            parse_duration("1w").unwrap(),
            Some(Duration::from_secs(604800))
        );
        assert_eq!(
            parse_duration("10").unwrap(),
            Some(Duration::from_secs(600))
        ); // default minutes

        // Negative & malformed edge-cases
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("-5m").is_err());
        // Overflow protection check
        assert!(parse_duration("18446744073709551615w").is_err());
        assert!(parse_duration("9999999999999999999w").is_err());
    }

    #[test]
    fn test_ban_lifecycle() {
        let mut reg = BanRegistry::new();
        let steam_id = "STEAM_0:1:98765432";

        assert!(reg.check(steam_id).is_none());

        reg.ban(
            steam_id,
            Some(Duration::from_secs(3600)),
            "Cheating",
            "Admin",
        );
        assert!(reg.check(steam_id).is_some());
        assert_eq!(reg.check(steam_id).unwrap().reason, "Cheating");

        assert!(reg.unban(steam_id));
        assert!(reg.check(steam_id).is_none());
    }

    #[test]
    fn test_mute_lifecycle() {
        let mut reg = MuteRegistry::new();
        assert!(!reg.is_muted(1));

        reg.mute(1, Some(Duration::from_secs(60)), "Mic spam");
        assert!(reg.is_muted(1));

        assert!(reg.unmute(1));
        assert!(!reg.is_muted(1));
    }

    #[test]
    fn test_moderation_context_alignment_and_blackboard() {
        let ctx = ModerationContext::new(Some(1), "Player1", "STEAM_0:1:12345", 0);
        assert_eq!(std::mem::align_of_val(&ctx), 64);
        assert!(!ctx.is_suppressed);
    }

    struct MockAuditLayer {
        called: std::sync::atomic::AtomicBool,
    }

    impl ModerationLayer for MockAuditLayer {
        fn on_enter(
            &self,
            _ctx: &mut ModerationContext<'_>,
            _intent: &ModerationIntent<'_>,
        ) -> FlowControl<(), (), ()> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            FlowControl::Proceed(())
        }

        fn on_exit(
            &self,
            _ctx: &mut ModerationContext<'_>,
            _intent: &ModerationIntent<'_>,
            _outcome: &mut Result<(), ()>,
        ) {
        }
    }

    #[test]
    fn test_moderation_pipeline_u_cycle() {
        let audit = Arc::new(MockAuditLayer {
            called: std::sync::atomic::AtomicBool::new(false),
        });

        let pipeline = ModerationPipeline::new().with_layer(audit.clone());
        let mut ctx = ModerationContext::new(Some(2), "Target", "STEAM_0:0:99", 0);
        let intent = ModerationIntent {
            kind: ModerationIntentKind::Kick,
            target: "Target",
            reason: "AFK",
            caller: 0,
        };

        let res = pipeline.dispatch(&mut ctx, &intent);
        assert_eq!(res, Ok(()));
        assert!(audit.called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
