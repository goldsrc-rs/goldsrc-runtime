//! Re-export of runtime menu session manager and renderers from `goldsrc-service-menu`.

pub use goldsrc_service_menu::*;

/// Initializes the menu action callback to dispatch engine events and client commands.
pub fn init_menu_hooks() {
    if let Ok(mut mgr) = menu_manager().lock() {
        mgr.set_action_handler(std::sync::Arc::new(|player_idx, item_id, action_name| {
            crate::hooks::emit(crate::host::HostEvent::MenuSelect {
                player: player_idx,
                item_id,
            });
            if !action_name.is_empty() {
                crate::hooks::dispatch_client_command(player_idx, action_name, "");
            }
        }));
    }
}
