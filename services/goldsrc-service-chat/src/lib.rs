//! Chat processing pipeline, triggers, recipient routing, and SMA middleware for GoldSrc.rs.

use goldsrc_api::chat::{
    ChatMessage, ChatScope, ChatTarget, LifeStateFilter, TeamTarget, split_chat_chunks,
};
use goldsrc_api::client::{LifeState, Player, Team};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use stitch_rs::flow::FlowControl;

/// Type definition for a chat filter middleware handler with SMA U-cycle semantics.
pub trait ChatLayer: Send + Sync {
    /// Descent phase: inspection, censorship, or short-circuiting triggers.
    fn on_enter(&self, msg: &mut ChatMessage) -> FlowControl<(), (), ()>;
    /// Ascent phase: audit logging, telemetry, or notification.
    fn on_exit(&self, msg: &mut ChatMessage, outcome: &mut Result<(), ()>);
}

/// Zero-cost adapter wrapping any [`ChatLayer`] to implement [`stitch_rs::middleware::Layer`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct ChatLayerAdapter<L>(pub L);

impl<L: ChatLayer> stitch_rs::middleware::Layer<ChatMessage, (), (), ()> for ChatLayerAdapter<L> {
    #[inline(always)]
    fn on_enter(&self, ctx: &mut ChatMessage, _intent: ()) -> FlowControl<(), (), ()> {
        self.0.on_enter(ctx)
    }

    #[inline(always)]
    fn on_exit(&self, ctx: &mut ChatMessage, outcome: &mut Result<(), ()>) {
        self.0.on_exit(ctx, outcome);
    }
}

/// Terminal leaf node terminating U-cycle descent for chat messages.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatTerminal;

impl stitch_rs::middleware::Terminal<ChatMessage, (), (), ()> for ChatTerminal {
    #[inline(always)]
    fn execute(&mut self, ctx: &mut ChatMessage, _intent: ()) -> Result<(), ()> {
        if ctx.is_blocked { Err(()) } else { Ok(()) }
    }
}

/// A monomorphic tuple-based U-cycle pipeline node for zero-cost static chat processing.
pub trait MonomorphicChatNode {
    /// Executes the full compile-time U-cycle on the given message.
    #[allow(clippy::result_unit_err)]
    fn cycle(&self, msg: &mut ChatMessage) -> Result<(), ()>;
}

impl MonomorphicChatNode for ChatTerminal {
    #[inline(always)]
    fn cycle(&self, msg: &mut ChatMessage) -> Result<(), ()> {
        if msg.is_blocked { Err(()) } else { Ok(()) }
    }
}

impl<L: ChatLayer, Next: MonomorphicChatNode> MonomorphicChatNode for (L, Next) {
    #[inline(always)]
    fn cycle(&self, msg: &mut ChatMessage) -> Result<(), ()> {
        match self.0.on_enter(msg) {
            FlowControl::Proceed(()) => {
                let mut outcome = self.1.cycle(msg);
                self.0.on_exit(msg, &mut outcome);
                outcome
            }
            FlowControl::ShortCircuit(()) => {
                let mut outcome = Ok(());
                self.0.on_exit(msg, &mut outcome);
                outcome
            }
            FlowControl::Halt(()) => {
                msg.is_blocked = true;
                let mut outcome = Err(());
                self.0.on_exit(msg, &mut outcome);
                outcome
            }
        }
    }
}

/// Runs a compile-time monomorphic U-cycle pipeline over [`ChatMessage`].
#[inline(always)]
#[allow(clippy::result_unit_err)]
pub fn run_monomorphic_chat_pipeline<N: MonomorphicChatNode>(
    node: &N,
    msg: &mut ChatMessage,
) -> Result<(), ()> {
    node.cycle(msg)
}

/// Global dynamic chat processing pipeline registry.
static CHAT_PIPELINE: LazyLock<RwLock<Vec<Arc<dyn ChatLayer>>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Pluggable resolver for custom chat target channels.
pub trait ChatTargetResolver: Send + Sync {
    /// Returns true if `recipient` is allowed to see messages sent to `channel` by `sender`.
    fn can_receive(&self, channel: &str, sender: Player, recipient: Player) -> bool;
}

impl<F> ChatTargetResolver for F
where
    F: Fn(&str, Player, Player) -> bool + Send + Sync,
{
    fn can_receive(&self, channel: &str, sender: Player, recipient: Player) -> bool {
        self(channel, sender, recipient)
    }
}

/// Global custom chat channel resolvers registry.
static CUSTOM_RESOLVERS: LazyLock<RwLock<HashMap<String, Arc<dyn ChatTargetResolver>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Registers a custom chat target channel resolver.
pub fn register_chat_target_resolver(channel: &str, resolver: Arc<dyn ChatTargetResolver>) {
    let mut map = CUSTOM_RESOLVERS.write().unwrap_or_else(|e| e.into_inner());
    map.insert(channel.to_ascii_lowercase(), resolver);
}

/// Unregisters a custom chat target channel resolver.
pub fn unregister_chat_target_resolver(channel: &str) {
    let mut map = CUSTOM_RESOLVERS.write().unwrap_or_else(|e| e.into_inner());
    map.remove(&channel.to_ascii_lowercase());
}

/// Registers an SMA chat layer in the global pipeline.
pub fn register_chat_layer(layer: Arc<dyn ChatLayer>) {
    let mut pipeline = match CHAT_PIPELINE.write() {
        Ok(p) => p,
        Err(e) => e.into_inner(),
    };
    pipeline.push(layer);
}

/// Context provider for dynamic command dispatching, external chat middleware, and placeholders.
pub trait ChatDispatcherContext {
    /// Dispatches a console or chat command. Returns true if handled.
    fn dispatch_command(&mut self, cmd: &str, sender_idx: i32, args: &str) -> bool {
        let _ = (cmd, sender_idx, args);
        false
    }

    /// Dispatches chat to external middleware (e.g. WASM plugins).
    /// Returns `Some(Some(transformed_text))` if modified, `Some(None)` if blocked, `None` if unhandled.
    fn dispatch_chat(
        &mut self,
        sender_idx: i32,
        text: &str,
        is_team: bool,
    ) -> Option<Option<String>> {
        let _ = (sender_idx, text, is_team);
        None
    }

    /// Evaluates placeholders in `template` for `caller`.
    fn format_placeholders(&mut self, template: &str, caller: Player) -> String {
        goldsrc_service_placeholders::format_placeholders(template, caller)
    }
}

/// Pluggable chat trigger that inspects raw incoming chat messages.
/// Subsystems (e.g. Command Dispatcher, Vote System, Keyword Interceptor)
/// register triggers to intercept and consume chat messages before they enter
/// the chat middleware pipeline.
pub trait ChatTrigger: Send + Sync {
    /// Attempts to handle the incoming chat message.
    /// Returns `true` if the message was handled and should be suppressed from server chat.
    fn try_handle(
        &self,
        context: Option<&mut dyn ChatDispatcherContext>,
        sender: Player,
        raw_text: &str,
    ) -> bool;
}

/// Global registry of chat message triggers.
static CHAT_TRIGGERS: LazyLock<RwLock<Vec<Arc<dyn ChatTrigger>>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Registers a custom chat trigger in the global registry.
pub fn register_chat_trigger(trigger: Arc<dyn ChatTrigger>) {
    let mut list = CHAT_TRIGGERS.write().unwrap_or_else(|e| e.into_inner());
    list.push(trigger);
}

/// Evaluates if an incoming raw chat message is consumed by any registered chat trigger.
pub fn evaluate_chat_triggers(
    mut context: Option<&mut (dyn ChatDispatcherContext + '_)>,
    sender: Player,
    raw_text: &str,
) -> bool {
    let triggers = match CHAT_TRIGGERS.read() {
        Ok(t) => t,
        Err(e) => e.into_inner(),
    };

    for trigger in triggers.iter() {
        let ctx = context
            .as_mut()
            .map(|c| &mut **c as &mut dyn ChatDispatcherContext);
        if trigger.try_handle(ctx, sender, raw_text) {
            return true;
        }
    }

    false
}

/// Standard chat trigger adapter routing chat-based commands (e.g. `/vip`, `!vip`, `rtv`)
/// to the registered command dispatcher context.
pub struct CommandChatTrigger;

impl ChatTrigger for CommandChatTrigger {
    fn try_handle(
        &self,
        mut context: Option<&mut dyn ChatDispatcherContext>,
        sender: Player,
        raw_text: &str,
    ) -> bool {
        let trimmed = raw_text.trim();
        if trimmed.is_empty() {
            return false;
        }

        let (first_word, rest) = match trimmed.find(|c: char| c.is_whitespace()) {
            Some(idx) => (&trimmed[..idx], trimmed[idx..].trim_start()),
            None => (trimmed, ""),
        };

        let clean_name = first_word.trim_start_matches(['/', '!']);

        if let Some(ref mut ctx) = context {
            ctx.dispatch_command(clean_name, sender.index(), rest)
        } else {
            false
        }
    }
}

/// Traversal runner for dynamic SMA U-cycle middleware layers.
#[allow(clippy::result_unit_err)]
pub fn run_chat_layers(layers: &[Arc<dyn ChatLayer>], msg: &mut ChatMessage) -> Result<(), ()> {
    let mut halted = false;
    let mut entered_count = 0;

    for layer in layers {
        match layer.on_enter(msg) {
            FlowControl::Proceed(()) => {
                entered_count += 1;
            }
            FlowControl::ShortCircuit(()) => {
                entered_count += 1;
                break;
            }
            FlowControl::Halt(()) => {
                halted = true;
                break;
            }
        }
    }

    let mut outcome = if halted || msg.is_blocked {
        Err(())
    } else {
        Ok(())
    };

    for layer in layers.iter().take(entered_count).rev() {
        layer.on_exit(msg, &mut outcome);
    }

    outcome
}

/// Dynamic or monomorphic chat middleware pipeline orchestrator conforming to SMA.
#[derive(Default, Clone)]
pub struct ChatPipeline {
    layers: Vec<Arc<dyn ChatLayer>>,
}

impl ChatPipeline {
    /// Creates a new empty chat middleware pipeline.
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// Appends a chat middleware layer to the pipeline.
    pub fn with_layer(mut self, layer: Arc<dyn ChatLayer>) -> Self {
        self.layers.push(layer);
        self
    }

    /// Dispatches an incoming [`ChatMessage`] through the pipeline's U-cycle.
    #[inline]
    #[allow(clippy::result_unit_err)]
    pub fn dispatch(&self, msg: &mut ChatMessage) -> Result<(), ()> {
        run_chat_layers(&self.layers, msg)
    }
}

/// Dispatches local chat middleware pipeline inside a guest plugin or standalone test.
pub fn dispatch_local_chat_middleware(
    sender_idx: i32,
    text: &str,
    is_team: bool,
) -> Option<String> {
    let sender = Player::new(sender_idx);
    let scope = if is_team {
        ChatScope::same_team()
    } else {
        ChatScope::all()
    };
    let mut msg = ChatMessage::new(sender, text, scope);
    let blocked = {
        let guard = match CHAT_PIPELINE.read() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        if guard.is_empty() {
            false
        } else {
            run_chat_layers(&guard, &mut msg).is_err()
        }
    };

    if blocked {
        return None;
    }

    if let Some(prefix) = msg.prefix {
        let mut out = String::with_capacity(prefix.len() + 16 + msg.formatted_text.len());
        out.push_str(&prefix);
        out.push_str("__PREFIX_SPLIT__");
        out.push_str(&msg.formatted_text);
        Some(out)
    } else {
        Some(msg.formatted_text)
    }
}

/// Dispatches an incoming `say` or `say_team` text command through the chat processing pipeline.
/// Broadcasts to eligible recipients matching the message's `ChatScope`.
/// Returns whether the message was handled and should be blocked from the vanilla engine.
pub fn process_chat_message(sender: Player, raw_text: &str, scope: ChatScope) -> bool {
    process_chat_message_with_context(None, sender, raw_text, scope)
}

/// Dispatches a chat message using an optional dispatcher context for external plugins & commands.
pub fn process_chat_message_with_context(
    mut context: Option<&mut (dyn ChatDispatcherContext + '_)>,
    sender: Player,
    raw_text: &str,
    scope: ChatScope,
) -> bool {
    let mut msg = ChatMessage::new(sender, raw_text, scope);

    // 0. Check pluggable chat triggers (consumed before entering chat pipeline)
    let ctx_triggers = context
        .as_mut()
        .map(|c| &mut **c as &mut dyn ChatDispatcherContext);
    if evaluate_chat_triggers(ctx_triggers, sender, raw_text) {
        return true;
    }

    // 1. Run SMA U-Cycle pipeline over registered layers (censorship, ranks, mute, custom prefixes)
    let blocked = {
        let guard = match CHAT_PIPELINE.read() {
            Ok(p) => p,
            Err(e) => e.into_inner(),
        };
        if guard.is_empty() {
            false
        } else {
            run_chat_layers(&guard, &mut msg).is_err()
        }
    };

    if blocked {
        return true; // blocked by middleware
    }

    // 2. Run external plugins chat middleware
    if let Some(ref mut ctx) = context {
        let is_team = msg.scope.is_team();
        let wasm_result = ctx.dispatch_chat(sender.index(), &msg.formatted_text, is_team);

        match wasm_result {
            Some(Some(transformed)) => {
                if let Some((prefix, rest)) = transformed.split_once("__PREFIX_SPLIT__") {
                    msg.prefix = Some(prefix.to_string());
                    msg.formatted_text = rest.to_string();
                } else {
                    msg.formatted_text = transformed;
                }
            }
            Some(None) => {
                return true; // blocked by middleware
            }
            None => {}
        }
    }

    // 3. Run placeholder expansion
    let interpolated = if let Some(ref mut ctx) = context {
        ctx.format_placeholders(&msg.formatted_text, sender)
    } else {
        goldsrc_service_placeholders::format_placeholders(&msg.formatted_text, sender)
    };
    msg.formatted_text = interpolated;

    // 4. Render final output with player name and prefix
    let sender_name = sender
        .get::<goldsrc_api::client::Name>()
        .as_deref()
        .map(String::from)
        .unwrap_or_else(|| format!("Player#{}", sender.index()));

    let full_text = match &msg.scope.target {
        ChatTarget::Team(TeamTarget::SameTeam) => {
            if let Some(ref prefix) = msg.prefix {
                format!(
                    "{prefix}^2(TEAM)^1 ^3{sender_name}^1 :  {}",
                    msg.formatted_text
                )
            } else {
                format!("^2(TEAM)^1 ^3{sender_name}^1 :  {}", msg.formatted_text)
            }
        }
        ChatTarget::Custom(channel) => {
            let ch_upper = channel.to_ascii_uppercase();
            if let Some(ref prefix) = msg.prefix {
                format!(
                    "{prefix}^4({ch_upper})^1 ^3{sender_name}^1 :  {}",
                    msg.formatted_text
                )
            } else {
                format!(
                    "^4({ch_upper})^1 ^3{sender_name}^1 :  {}",
                    msg.formatted_text
                )
            }
        }
        _ => {
            if let Some(ref prefix) = msg.prefix {
                format!("{prefix}^3{sender_name}^1 :  {}", msg.formatted_text)
            } else {
                format!("^3{sender_name}^1 :  {}", msg.formatted_text)
            }
        }
    };

    // 5. Split message into safe 180-byte chunks
    let chunks = split_chat_chunks(&full_text);

    // 6. Broadcast chunks to target recipients based on ChatScope
    let sender_team = sender.get::<goldsrc_api::client::Team>();
    match &msg.scope.target {
        ChatTarget::Direct(slot) => {
            let target = Player::new(*slot);
            if target.is_valid() && matches_lifestate(target, msg.scope.state) {
                for chunk in &chunks {
                    target.act(goldsrc_api::action::Print::chat(chunk));
                }
            }
        }
        ChatTarget::All | ChatTarget::Team(TeamTarget::All) => {
            for target in goldsrc_api::client::Players::all() {
                if matches_lifestate(target, msg.scope.state) {
                    for chunk in &chunks {
                        target.act(goldsrc_api::action::Print::chat(chunk));
                    }
                }
            }
        }
        ChatTarget::Team(TeamTarget::SameTeam) => {
            for target in goldsrc_api::client::Players::all() {
                if target.get::<goldsrc_api::client::Team>() == sender_team
                    && matches_lifestate(target, msg.scope.state)
                {
                    for chunk in &chunks {
                        target.act(goldsrc_api::action::Print::chat(chunk));
                    }
                }
            }
        }
        ChatTarget::Team(TeamTarget::OppositeTeam) => {
            for target in goldsrc_api::client::Players::all() {
                if is_opposite_team(sender_team, target.get::<goldsrc_api::client::Team>())
                    && matches_lifestate(target, msg.scope.state)
                {
                    for chunk in &chunks {
                        target.act(goldsrc_api::action::Print::chat(chunk));
                    }
                }
            }
        }
        ChatTarget::Custom(channel) => {
            let resolver = {
                let map = CUSTOM_RESOLVERS.read().unwrap_or_else(|e| e.into_inner());
                map.get(channel.as_ref()).cloned()
            };

            for target in goldsrc_api::client::Players::all() {
                if matches_lifestate(target, msg.scope.state)
                    && resolver
                        .as_ref()
                        .is_some_and(|r| r.can_receive(channel, sender, target))
                {
                    for chunk in &chunks {
                        target.act(goldsrc_api::action::Print::chat(chunk));
                    }
                }
            }
        }
    }

    true // Handled by GoldSrc.rs chat engine
}

fn matches_lifestate(player: Player, filter: LifeStateFilter) -> bool {
    match filter {
        LifeStateFilter::Any => true,
        LifeStateFilter::AliveOnly => {
            player.get::<goldsrc_api::client::LifeState>() == LifeState::Alive
        }
        LifeStateFilter::DeadOnly => {
            player.get::<goldsrc_api::client::LifeState>() != LifeState::Alive
        }
    }
}

fn is_opposite_team(a: Team, b: Team) -> bool {
    !a.is_unassigned() && !b.is_unassigned() && !a.is_spectator() && !b.is_spectator() && a != b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_chat_resolver_registration() {
        let channel = "admin_test";
        register_chat_target_resolver(
            channel,
            Arc::new(|_ch: &str, _sender: Player, recipient: Player| recipient.index() == 1),
        );

        let map = CUSTOM_RESOLVERS.read().unwrap();
        assert!(map.contains_key("admin_test"));
        let resolver = map.get("admin_test").unwrap();
        assert!(resolver.can_receive("admin_test", Player::new(2), Player::new(1)));
        assert!(!resolver.can_receive("admin_test", Player::new(2), Player::new(2)));
        drop(map);

        unregister_chat_target_resolver(channel);
        let map = CUSTOM_RESOLVERS.read().unwrap();
        assert!(!map.contains_key("admin_test"));
    }

    struct MockVoteTrigger;
    impl ChatTrigger for MockVoteTrigger {
        fn try_handle(
            &self,
            _context: Option<&mut dyn ChatDispatcherContext>,
            _sender: Player,
            raw_text: &str,
        ) -> bool {
            raw_text.trim().eq_ignore_ascii_case("rtv")
        }
    }

    #[test]
    fn test_chat_trigger_interception() {
        register_chat_trigger(Arc::new(MockVoteTrigger));
        let player = Player::new(1);

        // "rtv" should be intercepted and consumed by MockVoteTrigger
        assert!(evaluate_chat_triggers(None, player, "rtv"));
        assert!(evaluate_chat_triggers(None, player, "  rtv  "));

        // Normal chat should not be intercepted
        assert!(!evaluate_chat_triggers(None, player, "hello world"));
    }

    struct AuditLayer;
    impl ChatLayer for AuditLayer {
        fn on_enter(&self, _msg: &mut ChatMessage) -> FlowControl<(), (), ()> {
            FlowControl::Proceed(())
        }

        fn on_exit(&self, msg: &mut ChatMessage, outcome: &mut Result<(), ()>) {
            if outcome.is_err() {
                msg.formatted_text = "[CENSORED]".to_string();
            }
        }
    }

    struct CensorshipLayer;
    impl ChatLayer for CensorshipLayer {
        fn on_enter(&self, msg: &mut ChatMessage) -> FlowControl<(), (), ()> {
            if msg.raw_text.contains("badword") {
                msg.is_blocked = true;
                FlowControl::Halt(())
            } else if msg.raw_text.contains("vip_hello") {
                msg.prefix = Some("[VIP]".to_string());
                FlowControl::Proceed(())
            } else {
                FlowControl::Proceed(())
            }
        }

        fn on_exit(&self, _msg: &mut ChatMessage, _outcome: &mut Result<(), ()>) {}
    }

    #[test]
    fn test_monomorphic_chat_pipeline() {
        struct CensorshipLayer;
        impl ChatLayer for CensorshipLayer {
            fn on_enter(&self, msg: &mut ChatMessage) -> FlowControl<(), (), ()> {
                if msg.raw_text.contains("badword") {
                    FlowControl::Halt(())
                } else {
                    FlowControl::Proceed(())
                }
            }
            fn on_exit(&self, _msg: &mut ChatMessage, _outcome: &mut Result<(), ()>) {}
        }

        let pipeline = (CensorshipLayer, ChatTerminal);
        let sender = Player::new(1);

        let mut msg_clean = ChatMessage::new(sender, "Hello world", ChatScope::all());
        assert!(pipeline.cycle(&mut msg_clean).is_ok());

        let mut msg_bad = ChatMessage::new(sender, "badword here", ChatScope::all());
        assert!(pipeline.cycle(&mut msg_bad).is_err());
        assert!(msg_bad.is_blocked);
    }

    #[test]
    fn test_chat_layer_sma_u_cycle() {
        let layers: Vec<Arc<dyn ChatLayer>> = vec![Arc::new(AuditLayer), Arc::new(CensorshipLayer)];

        let mut msg_clean = ChatMessage::new(Player::new(1), "vip_hello server", ChatScope::all());
        let res_clean = run_chat_layers(&layers, &mut msg_clean);
        assert!(res_clean.is_ok());
        assert_eq!(msg_clean.prefix.as_deref(), Some("[VIP]"));

        let mut msg_blocked = ChatMessage::new(Player::new(1), "say badword", ChatScope::all());
        let res_blocked = run_chat_layers(&layers, &mut msg_blocked);
        assert!(res_blocked.is_err());
        assert_eq!(msg_blocked.formatted_text, "[CENSORED]");

        // Verify ChatPipeline builder dispatch
        let pipeline = ChatPipeline::new()
            .with_layer(Arc::new(AuditLayer))
            .with_layer(Arc::new(CensorshipLayer));
        let mut msg_pipeline =
            ChatMessage::new(Player::new(1), "vip_hello server", ChatScope::all());
        assert!(pipeline.dispatch(&mut msg_pipeline).is_ok());
        assert_eq!(msg_pipeline.prefix.as_deref(), Some("[VIP]"));

        // Verify ChatLayerAdapter with stitch_rs::Layer contract
        use stitch_rs::middleware::Layer;
        let adapter = ChatLayerAdapter(CensorshipLayer);
        let mut msg_adapt = ChatMessage::new(Player::new(1), "badword test", ChatScope::all());
        let flow = adapter.on_enter(&mut msg_adapt, ());
        assert!(matches!(flow, FlowControl::Halt(())));
    }
}
