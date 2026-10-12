//! Runtime Menu Session Manager, pagination router, and network renderers for GoldSrc.rs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use goldsrc_api::consts::log_targets;
use goldsrc_api::hud::{HudEffect, HudKind, HudMessage};
use goldsrc_api::menu::{
    ExitBehavior, Menu, MenuContext, MenuRendererKind, RenderedMenuPage, SlotAction,
};
use goldsrc_spi::engine::Engine;

use stitch_rs::flow::FlowControl;

/// Execution context for a player menu slot selection in the SMA U-cycle.
#[derive(Debug, Clone)]
pub struct MenuSelectContext {
    /// Slot index of the player performing selection.
    pub player_idx: i32,
    /// Selected numeric slot (1..=10).
    pub slot: u8,
    /// Timestamp of current engine frame.
    pub current_time: f32,
    /// Active slot action associated with this slot.
    pub action: Option<SlotAction>,
    /// Whether this selection should be suppressed/blocked.
    pub is_suppressed: bool,
}

impl MenuSelectContext {
    /// Creates a new `MenuSelectContext` from a player menu selection.
    pub fn new(player_idx: i32, slot: u8, current_time: f32, action: Option<SlotAction>) -> Self {
        Self {
            player_idx,
            slot,
            current_time,
            action,
            is_suppressed: false,
        }
    }

    /// Suppresses this menu selection from taking effect.
    pub fn suppress(&mut self) {
        self.is_suppressed = true;
    }
}

/// Type definition for a menu filter middleware layer with SMA U-cycle semantics.
pub trait MenuLayer: Send + Sync {
    /// Descent phase: inspection, anti-flood/spam, authorization.
    fn on_enter(&self, ctx: &mut MenuSelectContext) -> FlowControl<(), (), ()>;
    /// Ascent phase: audit logging, telemetry, or notification.
    fn on_exit(&self, ctx: &mut MenuSelectContext, outcome: &mut Result<(), ()>);
}

/// Zero-cost adapter wrapping any [`MenuLayer`] to implement [`stitch_rs::middleware::Layer`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct MenuLayerAdapter<L>(pub L);

impl<L: MenuLayer> stitch_rs::middleware::Layer<MenuSelectContext, (), (), ()>
    for MenuLayerAdapter<L>
{
    #[inline(always)]
    fn on_enter(&self, ctx: &mut MenuSelectContext, _intent: ()) -> FlowControl<(), (), ()> {
        self.0.on_enter(ctx)
    }

    #[inline(always)]
    fn on_exit(&self, ctx: &mut MenuSelectContext, outcome: &mut Result<(), ()>) {
        self.0.on_exit(ctx, outcome);
    }
}

/// Terminal leaf node terminating U-cycle descent for menu selection.
#[derive(Debug, Clone, Copy, Default)]
pub struct MenuTerminal;

impl stitch_rs::middleware::Terminal<MenuSelectContext, (), (), ()> for MenuTerminal {
    #[inline(always)]
    fn execute(&mut self, ctx: &mut MenuSelectContext, _intent: ()) -> Result<(), ()> {
        if ctx.is_suppressed { Err(()) } else { Ok(()) }
    }
}

/// Traversal runner for dynamic SMA U-cycle menu middleware layers.
#[allow(clippy::result_unit_err)]
pub fn run_menu_layers(
    layers: &[Arc<dyn MenuLayer>],
    ctx: &mut MenuSelectContext,
) -> Result<(), ()> {
    let mut halted = false;
    let mut entered_count = 0;

    for layer in layers {
        match layer.on_enter(ctx) {
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

    let mut outcome = if halted || ctx.is_suppressed {
        Err(())
    } else {
        Ok(())
    };

    for layer in layers.iter().take(entered_count).rev() {
        layer.on_exit(ctx, &mut outcome);
    }

    outcome
}

/// Dynamic or monomorphic menu middleware pipeline orchestrator conforming to SMA.
#[derive(Default, Clone)]
pub struct MenuPipeline {
    layers: Vec<Arc<dyn MenuLayer>>,
}

impl MenuPipeline {
    /// Creates a new empty menu middleware pipeline.
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// Appends a menu middleware layer to the pipeline.
    pub fn with_layer(mut self, layer: Arc<dyn MenuLayer>) -> Self {
        self.layers.push(layer);
        self
    }

    /// Dispatches an incoming [`MenuSelectContext`] through the pipeline's U-cycle.
    #[inline]
    #[allow(clippy::result_unit_err)]
    pub fn dispatch(&self, ctx: &mut MenuSelectContext) -> Result<(), ()> {
        run_menu_layers(&self.layers, ctx)
    }
}

/// Callback hook invoked when an interactive menu action is executed.
/// Arguments: `(player_idx, item_id, action_name)`.
pub type MenuActionHandler = Arc<dyn Fn(i32, u32, &str) + Send + Sync>;

/// Active menu session for a single connected player.
pub struct PlayerMenuSession {
    pub menu: Arc<Menu>,
    pub current_page: usize,
    pub history_stack: Vec<(Arc<Menu>, usize)>,
    pub rendered_page: Option<RenderedMenuPage>,
    pub expiry_time: Option<f32>,
}

type PendingParentSession = (Arc<Menu>, usize, Vec<(Arc<Menu>, usize)>);

/// Global session manager handling interactive player menus.
pub struct MenuSessionManager {
    sessions: HashMap<i32, PlayerMenuSession>,
    pending_parent: HashMap<i32, PendingParentSession>,
    debounce_tracker: HashMap<i32, f32>,
    cooldown_tracker: HashMap<(i32, u32), f32>,
    action_handler: Option<MenuActionHandler>,
    pipeline: MenuPipeline,
    round_number: u32,
    round_start_time: f32,
}

impl MenuSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            pending_parent: HashMap::new(),
            debounce_tracker: HashMap::new(),
            cooldown_tracker: HashMap::new(),
            action_handler: None,
            pipeline: MenuPipeline::new(),
            round_number: 1,
            round_start_time: 0.0,
        }
    }

    /// Attaches an SMA [`MenuPipeline`] to the session manager.
    pub fn with_pipeline(mut self, pipeline: MenuPipeline) -> Self {
        self.pipeline = pipeline;
        self
    }

    /// Registers a middleware layer in the manager's menu pipeline.
    pub fn add_layer(&mut self, layer: Arc<dyn MenuLayer>) {
        self.pipeline.layers.push(layer);
    }

    /// Sets the action execution callback hook.
    pub fn set_action_handler(&mut self, handler: MenuActionHandler) {
        self.action_handler = Some(handler);
    }

    /// Opens a menu for a player, pushing any currently active menu to the history stack.
    pub fn open_menu(
        &mut self,
        player_idx: i32,
        new_menu: Menu,
        engine: &dyn Engine,
        current_time: f32,
    ) {
        // Check required capability
        if let Some(ref cap) = new_menu.required_capability
            && !goldsrc_api::auth::Auth::has_capability(player_idx, cap)
        {
            engine.client_print(
                player_idx,
                goldsrc_spi::engine::MessageDest::One as i32,
                &format!("[Menu] Access denied: requires capability '{cap}'.\n"),
            );
            return;
        }

        // Build history stack if an existing session or pending parent exists
        let pending = self.pending_parent.remove(&player_idx);

        let history = if let Some((parent_menu, parent_page, parent_history)) = pending {
            let mut stack = parent_history;
            stack.push((parent_menu, parent_page));
            stack
        } else if let Some(existing) = self.sessions.remove(&player_idx) {
            let mut stack = existing.history_stack;
            stack.push((existing.menu, existing.current_page));
            stack
        } else {
            Vec::new()
        };

        let expiry = if new_menu.timeout_seconds > 0 {
            Some(current_time + new_menu.timeout_seconds as f32)
        } else {
            None
        };

        let mut session = PlayerMenuSession {
            menu: Arc::new(new_menu),
            current_page: 0,
            history_stack: history,
            rendered_page: None,
            expiry_time: expiry,
        };

        self.render_and_send(player_idx, &mut session, engine, current_time);
        self.sessions.insert(player_idx, session);
    }

    /// Dispatches a `menuselect <slot>` command (slot 1..=10).
    /// Returns `true` if handled (consumed).
    pub fn handle_menuselect(
        &mut self,
        player_idx: i32,
        slot: u8,
        engine: &dyn Engine,
        current_time: f32,
    ) -> bool {
        let Some(mut session) = self.sessions.remove(&player_idx) else {
            return false;
        };

        let Some(ref rendered) = session.rendered_page else {
            return false;
        };

        let action = match rendered.slots.get(&slot) {
            Some(act) => act.clone(),
            None => {
                // Invalid slot, re-insert session and keep menu
                self.sessions.insert(player_idx, session);
                return false;
            }
        };

        // 0. Execute SMA MenuPipeline U-cycle
        let mut select_ctx =
            MenuSelectContext::new(player_idx, slot, current_time, Some(action.clone()));
        if self.pipeline.dispatch(&mut select_ctx).is_err() {
            // Suppressed by middleware layer, re-insert session
            self.sessions.insert(player_idx, session);
            return false;
        }

        // 1. Menu global debounce anti-flood check
        if let Some(debounce_dur) = session.menu.debounce {
            let debounce_secs = debounce_dur.as_secs_f32();
            if let Some(&last) = self.debounce_tracker.get(&player_idx)
                && (current_time - last) < debounce_secs
            {
                self.sessions.insert(player_idx, session);
                return false;
            }
            self.debounce_tracker.insert(player_idx, current_time);
        }

        // 2. Item-level cooldown check
        if let SlotAction::Execute { id, .. } = action
            && let Some(item) = session.menu.items.iter().find(|i| match i.kind {
                goldsrc_api::menu::ItemKind::Action { id: item_id, .. } => item_id == id,
                _ => false,
            })
            && let Some((cooldown_dur, ref on_spam)) = item.cooldown
        {
            let cooldown_secs = cooldown_dur.as_secs_f32();
            if let Some(&last) = self.cooldown_tracker.get(&(player_idx, id))
                && (current_time - last) < cooldown_secs
            {
                match on_spam {
                    goldsrc_api::menu::AntiSpamAction::Ignore
                    | goldsrc_api::menu::AntiSpamAction::MakeInactive => {}
                    goldsrc_api::menu::AntiSpamAction::Feedback(fb) => {
                        let player = goldsrc_api::Player::new(player_idx);
                        if let Some((target, ref msg)) = fb.message {
                            player.act(goldsrc_api::action::Print {
                                target,
                                message: msg.clone(),
                            });
                        }
                        if let Some(ref sound) = fb.sound {
                            player.act(goldsrc_api::action::PlaySound::new(sound));
                        }
                    }
                    goldsrc_api::menu::AntiSpamAction::CloseMenu => {
                        Self::clear_client_menu(player_idx, engine);
                        return true;
                    }
                }
                self.sessions.insert(player_idx, session);
                return false;
            }
            self.cooldown_tracker.insert((player_idx, id), current_time);
        }

        match action {
            SlotAction::Execute {
                id,
                action_name,
                keep_open,
            } => {
                // Save parent session state in case action handler opens a submenu
                self.pending_parent.insert(
                    player_idx,
                    (
                        session.menu.clone(),
                        session.current_page,
                        session.history_stack.clone(),
                    ),
                );

                if keep_open {
                    // Re-render and keep session active
                    self.render_and_send(player_idx, &mut session, engine, current_time);
                    self.sessions.insert(player_idx, session);
                } else {
                    // Close menu on client
                    Self::clear_client_menu(player_idx, engine);
                }

                if let Some(ref handler) = self.action_handler {
                    handler(player_idx, id, &action_name);
                }
                true
            }
            SlotAction::PrevPage => {
                if session.current_page > 0 {
                    session.current_page -= 1;
                }
                self.render_and_send(player_idx, &mut session, engine, current_time);
                self.sessions.insert(player_idx, session);
                true
            }
            SlotAction::NextPage => {
                session.current_page += 1;
                self.render_and_send(player_idx, &mut session, engine, current_time);
                self.sessions.insert(player_idx, session);
                true
            }
            SlotAction::Exit => {
                match session.menu.exit_behavior {
                    ExitBehavior::CloseAll => {
                        Self::clear_client_menu(player_idx, engine);
                    }
                    ExitBehavior::PopParent => {
                        if let Some((parent_menu, parent_page)) = session.history_stack.pop() {
                            session.menu = parent_menu;
                            session.current_page = parent_page;
                            session.expiry_time = if session.menu.timeout_seconds > 0 {
                                Some(current_time + session.menu.timeout_seconds as f32)
                            } else {
                                None
                            };
                            self.render_and_send(player_idx, &mut session, engine, current_time);
                            self.sessions.insert(player_idx, session);
                        } else {
                            Self::clear_client_menu(player_idx, engine);
                        }
                    }
                    ExitBehavior::PopParentPage(target_page) => {
                        if let Some((parent_menu, _)) = session.history_stack.pop() {
                            let total_pages = {
                                let player = goldsrc_api::Player::new(player_idx);
                                let ctx = MenuContext {
                                    player_index: player_idx,
                                    round_number: self.round_number,
                                    round_time_elapsed: (current_time - self.round_start_time)
                                        .max(0.0),
                                    is_alive: {
                                        use goldsrc_api::EntityExt;
                                        player.health().is_alive()
                                    },
                                    players_count: goldsrc_api::auth::Auth::total_players().max(1)
                                        as u32,
                                };
                                parent_menu
                                    .render_page(&ctx, 0)
                                    .map(|p| p.total_pages)
                                    .unwrap_or(1)
                            };

                            let resolved_page = if target_page < 0 {
                                total_pages.saturating_sub(1)
                            } else {
                                (target_page as usize)
                                    .saturating_sub(1)
                                    .min(total_pages.saturating_sub(1))
                            };

                            session.menu = parent_menu;
                            session.current_page = resolved_page;
                            session.expiry_time = if session.menu.timeout_seconds > 0 {
                                Some(current_time + session.menu.timeout_seconds as f32)
                            } else {
                                None
                            };
                            self.render_and_send(player_idx, &mut session, engine, current_time);
                            self.sessions.insert(player_idx, session);
                        } else {
                            Self::clear_client_menu(player_idx, engine);
                        }
                    }
                }
                true
            }
            SlotAction::DenyFeedback(deny_action) => {
                match deny_action {
                    goldsrc_api::menu::DenyAction::Feedback(fb) => {
                        if let Some((target, ref msg)) = fb.message {
                            let player = goldsrc_api::Player::new(player_idx);
                            player.act(goldsrc_api::action::Print {
                                target,
                                message: msg.clone(),
                            });
                        }
                        if let Some(ref snd) = fb.sound {
                            engine.emit_sound(player_idx, 0, snd, 1.0, 0.8, 0, 100);
                        }
                    }
                    goldsrc_api::menu::DenyAction::Custom(cb) => {
                        cb(player_idx);
                    }
                    _ => {}
                }
                // Keep session open
                self.sessions.insert(player_idx, session);
                true
            }
            SlotAction::Noop => {
                // Keep session open
                self.sessions.insert(player_idx, session);
                true
            }
        }
    }

    /// Records raw ShowMenu keys mask sent directly from WASM plugins.
    pub fn on_raw_show_menu(&mut self, player_idx: i32, keys_mask: u16, timeout: i32) {
        if keys_mask == 0 {
            self.sessions.remove(&player_idx);
            return;
        }

        let mut slots_map = HashMap::new();
        for slot in 1..=10 {
            if (keys_mask & (1 << (slot - 1))) != 0 {
                slots_map.insert(
                    slot,
                    SlotAction::Execute {
                        id: slot as u32,
                        action_name: String::new(),
                        keep_open: false,
                    },
                );
            }
        }

        let rendered = RenderedMenuPage {
            text: String::new(),
            keys_mask,
            page_number: 1,
            total_pages: 1,
            slots: slots_map,
            timeout,
            renderer: MenuRendererKind::Text,
        };

        let session = PlayerMenuSession {
            menu: Arc::new(Menu::builder("").build()),
            current_page: 0,
            history_stack: Vec::new(),
            rendered_page: Some(rendered),
            expiry_time: if timeout > 0 {
                Some(timeout as f32)
            } else {
                None
            },
        };

        self.sessions.insert(player_idx, session);
    }

    /// Clears the active menu for a player.
    pub fn close_menu(&mut self, player_idx: i32, engine: &dyn Engine) {
        if self.sessions.remove(&player_idx).is_some() {
            Self::clear_client_menu(player_idx, engine);
        }
    }

    /// Cleans up session, pending parents, debounces, and cooldowns when player disconnects.
    pub fn on_disconnect(&mut self, player_idx: i32) {
        self.sessions.remove(&player_idx);
        self.pending_parent.remove(&player_idx);
        self.debounce_tracker.remove(&player_idx);
        self.cooldown_tracker.retain(|&(p, _), _| p != player_idx);
    }

    /// Clears all sessions, pending parents, and trackers on map change or server shutdown.
    pub fn on_map_change(&mut self) {
        self.sessions.clear();
        self.pending_parent.clear();
        self.debounce_tracker.clear();
        self.cooldown_tracker.clear();
        self.round_number = 1;
        self.round_start_time = 0.0;
    }

    /// Updates round state for condition checking.
    pub fn on_round_start(&mut self, round: u32, current_time: f32) {
        self.round_number = round;
        self.round_start_time = current_time;
    }

    /// Re-renders and sends the currently open menu for `player_idx` if present (e.g. on language change).
    pub fn refresh_player_menu(&mut self, player_idx: i32, engine: &dyn Engine, current_time: f32) {
        let round_number = self.round_number;
        let round_start_time = self.round_start_time;
        if let Some(session) = self.sessions.get_mut(&player_idx) {
            let player = goldsrc_api::Player::new(player_idx);
            use goldsrc_api::ClientExt;
            let lang = player.lang();
            Arc::make_mut(&mut session.menu).style = session.menu.style.clone().with_lang(&lang);
            Self::render_and_send_session(
                round_number,
                round_start_time,
                player_idx,
                session,
                engine,
                current_time,
            );
        }
    }

    /// Re-renders and sends all currently open menus for all connected players.
    pub fn refresh_all_menus(&mut self, engine: &dyn Engine, current_time: f32) {
        let round_number = self.round_number;
        let round_start_time = self.round_start_time;
        for (&player_idx, session) in self.sessions.iter_mut() {
            let player = goldsrc_api::Player::new(player_idx);
            use goldsrc_api::ClientExt;
            let lang = player.lang();
            Arc::make_mut(&mut session.menu).style = session.menu.style.clone().with_lang(&lang);
            Self::render_and_send_session(
                round_number,
                round_start_time,
                player_idx,
                session,
                engine,
                current_time,
            );
        }
    }

    /// Frame tick checking timeouts.
    pub fn tick_frame(&mut self, current_time: f32, engine: &dyn Engine) {
        let mut expired = Vec::new();
        for (&player_idx, session) in self.sessions.iter() {
            if let Some(expiry) = session.expiry_time
                && current_time >= expiry
            {
                expired.push(player_idx);
            }
        }

        for player_idx in expired {
            self.sessions.remove(&player_idx);
            Self::clear_client_menu(player_idx, engine);
        }
    }

    fn render_and_send(
        &self,
        player_idx: i32,
        session: &mut PlayerMenuSession,
        engine: &dyn Engine,
        current_time: f32,
    ) {
        Self::render_and_send_session(
            self.round_number,
            self.round_start_time,
            player_idx,
            session,
            engine,
            current_time,
        );
    }

    fn render_and_send_session(
        round_number: u32,
        round_start_time: f32,
        player_idx: i32,
        session: &mut PlayerMenuSession,
        engine: &dyn Engine,
        current_time: f32,
    ) {
        let is_alive = engine.entity_health(player_idx) > 0.0;
        let elapsed = if round_start_time > 0.0 && current_time >= round_start_time {
            current_time - round_start_time
        } else {
            0.0
        };

        let ctx = MenuContext {
            player_index: player_idx,
            round_number,
            round_time_elapsed: elapsed,
            is_alive,
            players_count: 1, // Fallback
        };

        if let Some(rendered) = session.menu.render_page(&ctx, session.current_page) {
            match &rendered.renderer {
                MenuRendererKind::Text => {
                    // Send ShowMenu user message with multipart chunking (192 byte limit)
                    Self::send_show_menu_chunked(
                        engine,
                        player_idx,
                        rendered.keys_mask as i32,
                        rendered.timeout,
                        &rendered.text,
                    );
                }
                MenuRendererKind::Dhud {
                    position,
                    color,
                    effect,
                } => {
                    // 1. Send DHUD message with full text
                    let hud_msg = goldsrc_api::hud::HudMessage {
                        text: rendered.text.clone(),
                        kind: goldsrc_api::hud::HudKind::Dhud,
                        color: *color,
                        color2: *color,
                        position: *position,
                        effect: *effect,
                    };
                    send_hud_message(engine, Some(player_idx), &hud_msg);

                    // 2. Send invisible ShowMenu with active keys mask to enable slot keypresses (1..0)
                    Self::send_show_menu_chunked(
                        engine,
                        player_idx,
                        rendered.keys_mask as i32,
                        rendered.timeout,
                        "",
                    );
                }
            }
            session.rendered_page = Some(rendered);
        }
    }

    /// Sends a `ShowMenu` message chunked across multiple packets using the GoldSrc `multipart` flag
    /// to avoid exceeding the engine's 192-byte UserMessage buffer limit.
    pub fn send_show_menu_chunked(
        engine: &dyn Engine,
        player_idx: i32,
        keys_mask: i32,
        timeout: i32,
        text: &str,
    ) {
        let show_menu_id = engine.reg_user_msg("ShowMenu", -1);
        if show_menu_id <= 0 || show_menu_id == 255 {
            log::warn!(
                target: log_targets::MENU,
                "Cannot send ShowMenu: invalid user message ID '{show_menu_id}' for player #{player_idx}"
            );
            return;
        }

        if text.is_empty() {
            engine.message_begin(
                goldsrc_spi::engine::MessageDest::One as i32,
                show_menu_id,
                None,
                Some(player_idx),
            );
            engine.write_short(keys_mask);
            engine.write_char(timeout);
            engine.write_byte(0); // multipart = 0
            engine.write_string("");
            engine.message_end();
            return;
        }

        let max_chunk = goldsrc_api::consts::MAX_SHOW_MENU_CHUNK_SIZE;
        let mut remaining = text;

        while !remaining.is_empty() {
            let chunk_len = if remaining.len() <= max_chunk {
                remaining.len()
            } else {
                let mut end = max_chunk;
                while end > 0 && !remaining.is_char_boundary(end) {
                    end -= 1;
                }
                if end == 0 {
                    remaining.chars().next().map(|c| c.len_utf8()).unwrap_or(1)
                } else {
                    end
                }
            };

            let chunk = &remaining[..chunk_len];
            remaining = &remaining[chunk_len..];
            let has_more = !remaining.is_empty();

            engine.message_begin(
                goldsrc_spi::engine::MessageDest::One as i32,
                show_menu_id,
                None,
                Some(player_idx),
            );
            engine.write_short(keys_mask);
            engine.write_char(timeout);
            engine.write_byte(if has_more { 1 } else { 0 }); // 1 = append, 0 = finish
            engine.write_string(chunk);
            engine.message_end();
        }
    }

    fn clear_client_menu(player_idx: i32, engine: &dyn Engine) {
        // 1. Clear ShowMenu (keys = 0)
        let show_menu_id = engine.reg_user_msg("ShowMenu", -1);
        if show_menu_id > 0 && show_menu_id != 255 {
            engine.message_begin(
                goldsrc_spi::engine::MessageDest::One as i32,
                show_menu_id,
                None,
                Some(player_idx),
            );
            engine.write_short(0); // keys = 0 closes the menu
            engine.write_char(0);
            engine.write_byte(0);
            engine.write_string("");
            engine.message_end();
        }

        // 2. Clear any active DHUD message immediately (send empty text with 0 duration)
        let clear_dhud = goldsrc_api::hud::HudMessage {
            text: String::new(),
            kind: goldsrc_api::hud::HudKind::Dhud,
            color: goldsrc_api::hud::HudColor::WHITE,
            color2: goldsrc_api::hud::HudColor::WHITE,
            position: goldsrc_api::hud::HudCoord::CENTER,
            effect: goldsrc_api::hud::HudEffect::FadeInOut {
                fade_in: 0.0,
                fade_out: 0.0,
                hold_time: 0.0,
            },
        };
        send_hud_message(engine, Some(player_idx), &clear_dhud);
    }
}

impl Default for MenuSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Serializes and sends a `HudMessage` to a target player (or all players if player_idx is None).
pub fn send_hud_message(engine: &dyn Engine, player_idx: Option<i32>, msg: &HudMessage) {
    let (dest, target_idx) = match player_idx {
        Some(idx) => (
            goldsrc_spi::engine::MessageDest::OneUnreliable as i32,
            Some(idx),
        ),
        None => (goldsrc_spi::engine::MessageDest::Broadcast as i32, None),
    };

    let (effect_val, fade_in, fade_out, hold_time, fx_time) = match msg.effect {
        HudEffect::FadeInOut {
            fade_in,
            fade_out,
            hold_time,
        } => (0, fade_in, fade_out, hold_time, 0.0),
        HudEffect::Flicker { fx_time, hold_time } => (1, 0.0, 0.0, hold_time, fx_time),
        HudEffect::Typewriter {
            char_time,
            fade_out,
            hold_time,
        } => (2, char_time, fade_out, hold_time, 0.0),
    };

    match msg.kind {
        HudKind::Classic { channel } => {
            let x_fixed = if msg.position.x < 0.0 {
                -1.0
            } else {
                msg.position.x
            };
            let y_fixed = if msg.position.y < 0.0 {
                -1.0
            } else {
                msg.position.y
            };

            let x_val = (x_fixed * 8192.0) as i32;
            let y_val = (y_fixed * 8192.0) as i32;
            let fade_in_val = (fade_in * 256.0) as i32;
            let fade_out_val = (fade_out * 256.0) as i32;
            let hold_time_val = (hold_time * 256.0) as i32;
            let fx_time_val = (fx_time * 256.0) as i32;

            engine.message_begin(dest, goldsrc_api::consts::SVC_TEMPENTITY, None, target_idx);
            engine.write_byte(goldsrc_api::consts::TE_TEXTMESSAGE as i32);
            engine.write_byte(channel as i32);
            engine.write_short(x_val);
            engine.write_short(y_val);
            engine.write_byte(effect_val);
            engine.write_byte(msg.color.r as i32);
            engine.write_byte(msg.color.g as i32);
            engine.write_byte(msg.color.b as i32);
            engine.write_byte(msg.color.a as i32);
            engine.write_byte(msg.color2.r as i32);
            engine.write_byte(msg.color2.g as i32);
            engine.write_byte(msg.color2.b as i32);
            engine.write_byte(msg.color2.a as i32);
            engine.write_short(fade_in_val);
            engine.write_short(fade_out_val);
            engine.write_short(hold_time_val);
            if effect_val == 2 {
                engine.write_short(fx_time_val);
            }
            engine.write_string(&msg.text);
            engine.message_end();
        }
        HudKind::Dhud => {
            const SVC_DIRECTOR: i32 = 51;
            const DRC_CMD_MESSAGE: i32 = 6;

            let text_bytes = msg.text.as_bytes();
            let len = text_bytes.len().min(128);
            let safe_text = &msg.text[..len];

            let packed_color =
                (msg.color.b as i32) | ((msg.color.g as i32) << 8) | ((msg.color.r as i32) << 16);

            engine.message_begin(dest, SVC_DIRECTOR, None, target_idx);
            engine.write_byte((len as i32) + 31);
            engine.write_byte(DRC_CMD_MESSAGE);
            engine.write_byte(effect_val);
            engine.write_long(packed_color);
            engine.write_long(msg.position.x.to_bits() as i32);
            engine.write_long(msg.position.y.to_bits() as i32);
            engine.write_long(fade_in.to_bits() as i32);
            engine.write_long(fade_out.to_bits() as i32);
            engine.write_long(hold_time.to_bits() as i32);
            engine.write_long(fx_time.to_bits() as i32);
            engine.write_string(safe_text);
            engine.message_end();
        }
    }
}

static G_MENU_MANAGER: OnceLock<Mutex<MenuSessionManager>> = OnceLock::new();

pub fn menu_manager() -> &'static Mutex<MenuSessionManager> {
    G_MENU_MANAGER.get_or_init(|| Mutex::new(MenuSessionManager::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use goldsrc_api::cvar::{CvarEngine, CvarFlags};
    use goldsrc_api::entity::EntitySpawner;
    use goldsrc_api::menu::{MenuItem, MenuStyle};
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
            1
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
            true
        }
        fn entity_classname(&self, _index: i32) -> Option<String> {
            None
        }
        fn entity_health(&self, _index: i32) -> f32 {
            100.0
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
            Some("Tester".into())
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
    impl CvarEngine for MockEngine {
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
    fn test_menu_manager_open_and_navigation() {
        let mut mgr = MenuSessionManager::new();
        let engine = MockEngine;
        let player = 42;

        let menu = Menu::builder("Multi Page Menu")
            .style(MenuStyle::brackets())
            .item(MenuItem::new("Item 1", 101).keep_open())
            .item(MenuItem::new("Item 2", 102))
            .page(|p| p.item(MenuItem::new("Item 3", 103)))
            .build();

        mgr.open_menu(player, menu, &engine, 0.0);
        assert!(mgr.sessions.contains_key(&player));
        assert_eq!(mgr.sessions.get(&player).unwrap().current_page, 0);

        // NextPage (slot 9)
        let handled = mgr.handle_menuselect(player, 9, &engine, 0.5);
        assert!(handled);
        assert_eq!(mgr.sessions.get(&player).unwrap().current_page, 1);

        // Select item 3 (slot 1) on page 1 (not keep_open)
        let handled = mgr.handle_menuselect(player, 1, &engine, 1.0);
        assert!(handled);
        // Session should be closed
        assert!(!mgr.sessions.contains_key(&player));
    }

    #[test]
    fn test_menu_manager_cooldown() {
        let mut mgr = MenuSessionManager::new();
        let engine = MockEngine;
        let player = 43;

        let menu = Menu::builder("Spam Test")
            .item(
                MenuItem::new("Spam Item", 200)
                    .keep_open()
                    .cooldown(std::time::Duration::from_secs(2)),
            )
            .build();

        mgr.open_menu(player, menu, &engine, 10.0);

        // First click at t=10.5 should succeed
        assert!(mgr.handle_menuselect(player, 1, &engine, 10.5));

        // Second click at t=11.0 is within 2.0s cooldown -> suppressed
        assert!(!mgr.handle_menuselect(player, 1, &engine, 11.0));

        // Third click at t=13.0 is after cooldown -> succeeds
        assert!(mgr.handle_menuselect(player, 1, &engine, 13.0));
    }

    #[test]
    fn test_menu_manager_expiry_timeout() {
        let mut mgr = MenuSessionManager::new();
        let engine = MockEngine;
        let player = 44;

        let mut menu = Menu::builder("Timed Menu")
            .item(("Do Something", 300))
            .build();
        menu.timeout_seconds = 5;

        mgr.open_menu(player, menu, &engine, 100.0);
        assert!(mgr.sessions.contains_key(&player));

        // Frame tick at t=102.0 -> still alive
        mgr.tick_frame(102.0, &engine);
        assert!(mgr.sessions.contains_key(&player));

        // Frame tick at t=106.0 -> expired and removed
        mgr.tick_frame(106.0, &engine);
        assert!(!mgr.sessions.contains_key(&player));
    }

    struct RejectSlotOneLayer;
    impl MenuLayer for RejectSlotOneLayer {
        fn on_enter(&self, ctx: &mut MenuSelectContext) -> FlowControl<(), (), ()> {
            if ctx.slot == 1 {
                ctx.suppress();
                FlowControl::Halt(())
            } else {
                FlowControl::Proceed(())
            }
        }

        fn on_exit(&self, _ctx: &mut MenuSelectContext, _outcome: &mut Result<(), ()>) {}
    }

    #[test]
    fn test_menu_pipeline_sma_u_cycle() {
        let mut mgr = MenuSessionManager::new();
        let engine = MockEngine;
        let player = 45;

        mgr.add_layer(Arc::new(RejectSlotOneLayer));

        let menu = Menu::builder("Pipeline Menu")
            .item(("Action 1", 101))
            .item(("Action 2", 102))
            .build();

        mgr.open_menu(player, menu, &engine, 50.0);

        // Slot 1 is blocked by RejectSlotOneLayer
        assert!(!mgr.handle_menuselect(player, 1, &engine, 50.5));

        // Slot 2 proceeds and succeeds
        assert!(mgr.handle_menuselect(player, 2, &engine, 51.0));
    }
}
