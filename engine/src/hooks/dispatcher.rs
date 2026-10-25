//! Centralized event and command dispatcher for backends and WASM plugins.

use crate::host::{HostEvent, HostRuntime};
use goldsrc_api::consts::log_targets;

/// Dispatches any engine event to the host runtime and all loaded WASM plugins.
/// Returns `true` if the host runtime is active and processed the event.
pub fn emit(event: HostEvent<'_>) -> bool {
    // 1. CQS internal runtime reaction
    HostRuntime::on(event);

    // 2. Deliver to WASM plugins
    let name = event.name();
    let payload = event.payload();
    HostRuntime::with_manager(|m| match m {
        Some(manager) => {
            manager.call_on_event(name, &payload);
            true
        }
        None => {
            log::trace!(target: log_targets::CORE, "emit('{name}') skipped: WASM host not initialized");
            false
        }
    })
}

/// Dispatches a console / client command to the WASM host.
/// Returns `true` if the host runtime is active and processed the command.
pub fn dispatch_command(cmd: &str, args: &str) -> bool {
    HostRuntime::with_manager(|m| match m {
        Some(manager) => manager.dispatch_command(cmd, 0, args),
        None => {
            log::trace!(target: log_targets::CORE, "dispatch_command('{cmd}') skipped: WASM host not initialized");
            false
        }
    })
}

/// Dispatches a client command (including chat commands e.g. `say /vip` or console `vipmenu`).
/// Returns `true` if a plugin intercepted and handled the command (requesting suppression from GameDLL).
pub fn dispatch_client_command(player_idx: i32, cmd: &str, raw_args: &str) -> bool {
    // 1. Check for `menuselect <slot>` client command (slot 1..=10)
    if cmd.eq_ignore_ascii_case("menuselect") {
        let slot = raw_args.trim().parse::<u8>().unwrap_or(0);
        let slot = if slot == 0 { 10 } else { slot };

        if let Some(engine) = HostRuntime::engine() {
            let current_time = HostRuntime::current_time();
            if let Ok(mut mgr) = crate::menu::menu_manager().lock()
                && mgr.handle_menuselect(player_idx, slot, engine.as_ref(), current_time)
            {
                return true;
            }
        }

        // Fallback: dispatch raw slot to WASM plugins event "menu_select" (8 bytes payload: [player_idx: i32, slot: u32])
        let mut payload = [0u8; 8];
        payload[0..4].copy_from_slice(&player_idx.to_le_bytes());
        payload[4..8].copy_from_slice(&(slot as u32).to_le_bytes());

        let targeted = if let Some(owner) = goldsrc_host_wasm::get_active_menu_owner(player_idx) {
            HostRuntime::with_manager(|m| {
                if let Some(manager) = m {
                    manager.call_plugin_event(&owner, "menu_select", &payload)
                } else {
                    false
                }
            })
        } else {
            false
        };

        if !targeted {
            emit(HostEvent::MenuSelect {
                player: player_idx,
                item_id: slot as u32,
            });
        }

        return true;
    }

    HostRuntime::with_manager(|m| {
        let Some(manager) = m else {
            return false;
        };

        if cmd.eq_ignore_ascii_case("say") || cmd.eq_ignore_ascii_case("say_team") {
            let mut text = raw_args.trim();
            if text.starts_with('"') && text.ends_with('"') && text.len() >= 2 {
                text = &text[1..text.len() - 1];
            }
            if text.trim().is_empty() {
                // Suppress empty chat messages
                return true;
            }

            // Route standard player chat directly through the chat pipeline
            let sender = goldsrc_api::client::Player::new(player_idx);
            let scope = if cmd.eq_ignore_ascii_case("say_team") {
                goldsrc_api::chat::ChatScope::same_team()
            } else {
                goldsrc_api::chat::ChatScope::all()
            };
            return crate::chat::process_chat_message_with_manager(
                Some(manager),
                sender,
                text,
                scope,
            );
        }

        // Direct client console command
        manager.dispatch_command(cmd, player_idx, raw_args)
    })
}
