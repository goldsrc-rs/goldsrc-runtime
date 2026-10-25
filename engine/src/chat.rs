//! Re-export of chat processing pipeline from `goldsrc-service-chat` with host WASM context integration.

pub use goldsrc_service_chat::*;

/// Adapter wrapping `PluginManager` into `ChatDispatcherContext`.
pub struct PluginManagerChatDispatcher<'a>(pub &'a mut goldsrc_host_wasm::PluginManager);

/// Backward compatibility alias for `PluginManagerChatDispatcher`.
pub type PluginManagerChatContext<'a> = PluginManagerChatDispatcher<'a>;

impl<'a> ChatDispatcherContext for PluginManagerChatDispatcher<'a> {
    fn dispatch_command(&mut self, cmd: &str, sender_idx: i32, args: &str) -> bool {
        self.0.dispatch_command(cmd, sender_idx, args)
    }

    fn dispatch_chat(
        &mut self,
        sender_idx: i32,
        text: &str,
        is_team: bool,
    ) -> Option<Option<String>> {
        Some(self.0.dispatch_chat(sender_idx, text, is_team))
    }

    fn format_placeholders(
        &mut self,
        template: &str,
        caller: goldsrc_api::client::Player,
    ) -> String {
        crate::placeholders::format_placeholders_with_manager(template, caller, Some(self.0))
    }
}

struct HostRuntimeChatDispatcher;

impl ChatDispatcherContext for HostRuntimeChatDispatcher {
    fn dispatch_command(&mut self, cmd: &str, sender_idx: i32, args: &str) -> bool {
        crate::host::HostRuntime::with_manager(|mgr| {
            mgr.map(|m| m.dispatch_command(cmd, sender_idx, args))
                .unwrap_or(false)
        })
    }

    fn dispatch_chat(
        &mut self,
        sender_idx: i32,
        text: &str,
        is_team: bool,
    ) -> Option<Option<String>> {
        crate::host::HostRuntime::with_manager(|mgr| {
            mgr.map(|m| m.dispatch_chat(sender_idx, text, is_team))
        })
    }

    fn format_placeholders(
        &mut self,
        template: &str,
        caller: goldsrc_api::client::Player,
    ) -> String {
        crate::host::HostRuntime::with_manager(|mgr| {
            crate::placeholders::format_placeholders_with_manager(template, caller, mgr)
        })
    }
}

/// Dispatches an incoming `say` or `say_team` text command through the chat processing pipeline.
/// Uses the active HostRuntime WASM manager context if available.
pub fn process_chat_message(
    sender: goldsrc_api::client::Player,
    raw_text: &str,
    scope: goldsrc_api::chat::ChatScope,
) -> bool {
    let mut ctx = HostRuntimeChatDispatcher;
    goldsrc_service_chat::process_chat_message_with_context(Some(&mut ctx), sender, raw_text, scope)
}

/// Dispatches a chat message using an optional pre-locked `PluginManager` to avoid re-entrant mutex deadlocks.
pub fn process_chat_message_with_manager(
    manager: Option<&mut goldsrc_host_wasm::PluginManager>,
    sender: goldsrc_api::client::Player,
    raw_text: &str,
    scope: goldsrc_api::chat::ChatScope,
) -> bool {
    if let Some(mgr) = manager {
        let mut ctx = PluginManagerChatDispatcher(mgr);
        goldsrc_service_chat::process_chat_message_with_context(
            Some(&mut ctx),
            sender,
            raw_text,
            scope,
        )
    } else {
        process_chat_message(sender, raw_text, scope)
    }
}

/// Evaluates if an incoming raw chat message is consumed by any registered chat trigger.
pub fn evaluate_chat_triggers(
    manager: Option<&mut goldsrc_host_wasm::PluginManager>,
    sender: goldsrc_api::client::Player,
    raw_text: &str,
) -> bool {
    if let Some(mgr) = manager {
        let mut ctx = PluginManagerChatDispatcher(mgr);
        goldsrc_service_chat::evaluate_chat_triggers(Some(&mut ctx), sender, raw_text)
    } else {
        let mut ctx = HostRuntimeChatDispatcher;
        goldsrc_service_chat::evaluate_chat_triggers(Some(&mut ctx), sender, raw_text)
    }
}

/// Formats a chat string with placeholder evaluation and arguments.
#[macro_export]
macro_rules! chat_format {
    ($fmt:expr) => {{
        $fmt.to_string()
    }};
    ($fmt:expr, $( $arg:tt )*) => {{
        format!($fmt, $( $arg )*)
    }};
}

/// Prints a formatted colored message to a specific player with safe chunk splitting.
#[macro_export]
macro_rules! chat_print {
    ($target:expr, $msg:literal) => {{
        let player = $crate::Player::from($target);
        let formatted = $crate::placeholders::format_placeholders($msg, player);
        let chunks = $crate::goldsrc_api::chat::split_chat_chunks(&formatted);
        for chunk in chunks {
            player.act($crate::goldsrc_api::action::Print::chat(&chunk));
        }
    }};
    ($target:expr, $fmt:expr, $( $arg:expr ),* $(,)?) => {{
        let text = format!($fmt, $( $arg ),*);
        let player = $crate::Player::from($target);
        let formatted = $crate::placeholders::format_placeholders(&text, player);
        let chunks = $crate::goldsrc_api::chat::split_chat_chunks(&formatted);
        for chunk in chunks {
            player.act($crate::goldsrc_api::action::Print::chat(&chunk));
        }
    }};
}

/// Broadcasts a formatted colored message to all connected players with safe chunk splitting.
#[macro_export]
macro_rules! chat_broadcast {
    ($msg:literal) => {{
        for player in $crate::goldsrc_api::client::Players::all() {
            let formatted = $crate::placeholders::format_placeholders($msg, player);
            let chunks = $crate::goldsrc_api::chat::split_chat_chunks(&formatted);
            for chunk in chunks {
                player.act($crate::goldsrc_api::action::Print::chat(&chunk));
            }
        }
    }};
    ($fmt:expr, $( $arg:expr ),* $(,)?) => {{
        let text = format!($fmt, $( $arg ),*);
        for player in $crate::goldsrc_api::client::Players::all() {
            let formatted = $crate::placeholders::format_placeholders(&text, player);
            let chunks = $crate::goldsrc_api::chat::split_chat_chunks(&formatted);
            for chunk in chunks {
                player.act($crate::goldsrc_api::action::Print::chat(&chunk));
            }
        }
    }};
}

/// Broadcasts a formatted colored message to all teammates of the sender with safe chunk splitting.
#[macro_export]
macro_rules! chat_team {
    ($sender:expr, $msg:literal) => {{
        let sender = $crate::Player::from($sender);
        let sender_team = sender.get::<$crate::goldsrc_api::Team>();
        for player in $crate::goldsrc_api::client::Players::all() {
            if player.get::<$crate::goldsrc_api::Team>() == sender_team {
                let formatted = $crate::placeholders::format_placeholders($msg, player);
                let chunks = $crate::goldsrc_api::chat::split_chat_chunks(&formatted);
                for chunk in chunks {
                    player.act($crate::goldsrc_api::action::Print::chat(&chunk));
                }
            }
        }
    }};
    ($sender:expr, $fmt:expr, $( $arg:expr ),* $(,)?) => {{
        let text = format!($fmt, $( $arg ),*);
        let sender = $crate::Player::from($sender);
        let sender_team = sender.get::<$crate::goldsrc_api::Team>();
        for player in $crate::goldsrc_api::client::Players::all() {
            if player.get::<$crate::goldsrc_api::Team>() == sender_team {
                let formatted = $crate::placeholders::format_placeholders(&text, player);
                let chunks = $crate::goldsrc_api::chat::split_chat_chunks(&formatted);
                for chunk in chunks {
                    player.act($crate::goldsrc_api::action::Print::chat(&chunk));
                }
            }
        }
    }};
}
