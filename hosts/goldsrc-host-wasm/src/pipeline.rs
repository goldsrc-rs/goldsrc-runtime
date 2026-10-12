//! Zero-Cost U-Cycle Middleware Pipelines for GoldSrc WASM Host using `stitch-rs`.
//!
//! Provides U-cycle processing pipelines for chat messages, console commands,
//! and engine events following The Sewing Machine Architecture (SMA).

use crate::plugin::LoadedPlugin;

/// Context passing through the chat processing U-cycle.
#[repr(C, align(64))]
pub struct ChatContext<'a> {
    pub current_text: &'a mut String,
    pub sender: i32,
    pub is_team: bool,
}

impl<'a> stitch_rs::Blackboard for ChatContext<'a> {}

/// The intent entering the chat pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatIntent<'a> {
    pub text: &'a str,
}

/// Reason for chat rejection or suppression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatDropReason {
    SuppressedByPlugin(String),
    Muted,
    Censored,
}

/// Dynamic SMA chat pipeline runner.
/// Executes registered plugin interceptors in U-cycle traversal order without heap allocations on hot path.
pub fn execute_chat_pipeline(
    plugins: &mut [LoadedPlugin],
    ctx: &mut ChatContext<'_>,
    _intent: ChatIntent<'_>,
) -> Result<(), ChatDropReason> {
    let mut halted = None;

    // Phase 1: Descent (on_enter)
    for plugin in plugins.iter_mut() {
        if !plugin.has_export("on-chat") {
            continue;
        }

        match plugin.call_on_chat(ctx.sender, ctx.current_text, ctx.is_team) {
            Ok(Some(transformed)) => {
                *ctx.current_text = transformed;
            }
            Ok(None) => {
                halted = Some(ChatDropReason::SuppressedByPlugin(plugin.name.clone()));
                break;
            }
            Err(e) => {
                log::warn!(
                    target: goldsrc_api::consts::log_targets::WASM,
                    "Error executing on-chat in plugin '{}': {e}",
                    plugin.name
                );
            }
        }
    }

    if let Some(err) = halted {
        Err(err)
    } else {
        Ok(())
    }
}

/// Context passing through the command dispatch U-cycle.
#[repr(C, align(64))]
pub struct CommandContext<'a> {
    pub cmd: &'a str,
    pub caller: i32,
    pub args: &'a str,
    pub consumed: bool,
}

impl<'a> stitch_rs::Blackboard for CommandContext<'a> {}

/// Outcome error for command dispatch failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandDispatchError {
    ExecutionFailed(String),
}

/// Dynamic SMA command pipeline runner.
/// Dispatches a registered command through candidate plugins in U-cycle traversal.
pub fn execute_command_pipeline(
    plugins: &mut [LoadedPlugin],
    handler_indices: &[usize],
    ctx: &mut CommandContext<'_>,
) -> bool {
    for &idx in handler_indices {
        if let Some(plugin) = plugins.get_mut(idx) {
            match plugin.call_on_command(ctx.cmd, ctx.caller, ctx.args) {
                Ok(true) => {
                    ctx.consumed = true;
                    break;
                }
                Ok(false) => {}
                Err(e) => {
                    log::warn!(
                        target: goldsrc_api::consts::log_targets::WASM,
                        "Command '{}' invocation failed in plugin '{}': {e}",
                        ctx.cmd,
                        plugin.name
                    );
                }
            }
        }
    }

    ctx.consumed
}

/// Context passing through the engine event broadcast U-cycle.
#[repr(C, align(64))]
pub struct EventContext<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    pub had_poison: bool,
    pub delivered_count: usize,
}

impl<'a> stitch_rs::Blackboard for EventContext<'a> {}

/// Dynamic SMA event broadcast pipeline runner.
/// Propagates an engine/game event across all active plugins with safe poison tracking.
pub fn execute_event_pipeline(plugins: &mut [LoadedPlugin], ctx: &mut EventContext<'_>) {
    for plugin in plugins.iter_mut() {
        if !plugin.status.is_executable() {
            continue;
        }

        match plugin.call_on_event(ctx.name, ctx.data) {
            Ok(()) => {
                ctx.delivered_count += 1;
            }
            Err(e) => {
                ctx.had_poison = true;
                log::error!(
                    target: goldsrc_api::consts::log_targets::WASM,
                    "Event '{}' dispatch failed in plugin '{}': {e}",
                    ctx.name,
                    plugin.name
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blackboard_and_contexts_layout() {
        let mut text = String::from("hello");
        let chat_ctx = ChatContext {
            current_text: &mut text,
            sender: 1,
            is_team: false,
        };
        assert_eq!(std::mem::align_of_val(&chat_ctx), 64);

        let cmd_ctx = CommandContext {
            cmd: "test",
            caller: 0,
            args: "arg1",
            consumed: false,
        };
        assert_eq!(std::mem::align_of_val(&cmd_ctx), 64);

        let evt_ctx = EventContext {
            name: "round_start",
            data: b"payload",
            had_poison: false,
            delivered_count: 0,
        };
        assert_eq!(std::mem::align_of_val(&evt_ctx), 64);
    }

    #[test]
    fn test_execute_command_pipeline_empty_plugins() {
        let mut plugins = Vec::new();
        let mut ctx = CommandContext {
            cmd: "amx_help",
            caller: 1,
            args: "",
            consumed: false,
        };
        let res = execute_command_pipeline(&mut plugins, &[], &mut ctx);
        assert!(!res);
        assert!(!ctx.consumed);
    }

    #[test]
    fn test_execute_event_pipeline_empty_plugins() {
        let mut plugins = Vec::new();
        let mut ctx = EventContext {
            name: "server_spawn",
            data: b"",
            had_poison: false,
            delivered_count: 0,
        };
        execute_event_pipeline(&mut plugins, &mut ctx);
        assert!(!ctx.had_poison);
        assert_eq!(ctx.delivered_count, 0);
    }
}
