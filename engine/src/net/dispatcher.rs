//! Network message dispatcher for packing and transmitting GoldSrc engine user messages.
//!
//! Encapsulates low-level byte-packing protocols for `TextMsg`, `SayText`, HUD messages,
//! and ensures strict adherence to GoldSrc buffer boundaries (185 bytes) and UTF-8 safety.

use goldsrc_api::consts::SAFE_SAYTEXT_LIMIT;
use goldsrc_api::{HUD_PRINTCENTER, HUD_PRINTCONSOLE, HUD_PRINTNOTIFY, PrintTarget};
use goldsrc_spi::engine::MessageDest;

/// Dispatcher responsible for formatting, chunking, and sending GoldSrc network user messages.
pub struct NetworkMessageDispatcher;

impl NetworkMessageDispatcher {
    /// Dispatches a formatted message to a player according to the target print channel.
    pub fn dispatch_player_print(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        player_index: i32,
        target: PrintTarget,
        message: &str,
    ) {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&player_index)
            || !engine.entity_is_valid(player_index)
        {
            return;
        }

        match target {
            PrintTarget::Console | PrintTarget::Notify | PrintTarget::Center => {
                let (msg_dest, formatted) = match target {
                    PrintTarget::Console => (
                        HUD_PRINTCONSOLE,
                        if message.ends_with('\n') {
                            message.to_string()
                        } else {
                            format!("{message}\n")
                        },
                    ),
                    PrintTarget::Notify => {
                        (HUD_PRINTNOTIFY, goldsrc_api::format_notify_text(message))
                    }
                    PrintTarget::Center => {
                        (HUD_PRINTCENTER, goldsrc_api::format_center_text(message))
                    }
                    _ => unreachable!(),
                };

                Self::send_text_msg(engine, player_index, msg_dest, &formatted);
            }
            PrintTarget::Chat => {
                Self::send_say_text(engine, player_index, player_index, message);
            }
        }
    }

    /// Sends a `TextMsg` user message (console, notify, center text) to a single client.
    pub fn send_text_msg(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        player_index: i32,
        msg_dest: i32,
        formatted: &str,
    ) {
        let mut payload = formatted.to_string();
        // AMX Mod X protocol: if format string is used, double newline is needed for notify/console in cstrike
        if (msg_dest == HUD_PRINTNOTIFY || msg_dest == HUD_PRINTCONSOLE)
            && !payload.ends_with("\n\n")
        {
            payload.push('\n');
        }
        Self::send_text_msg_preformatted(engine, player_index, msg_dest, &payload);
    }

    /// Sends a preformatted `TextMsg` payload directly to the wire, avoiding redundant allocations.
    pub fn send_text_msg_preformatted(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        player_index: i32,
        msg_dest: i32,
        payload: &str,
    ) {
        let text_msg_id = engine.reg_user_msg("TextMsg", -1);
        if text_msg_id > 0 && text_msg_id < 255 {
            let safe_msg = if payload.len() > 185 {
                let mut end = 185;
                while end > 0 && !payload.is_char_boundary(end) {
                    end -= 1;
                }
                &payload[..end]
            } else {
                payload
            };

            engine.message_begin(
                MessageDest::One as i32,
                text_msg_id,
                None,
                Some(player_index),
            );
            engine.write_byte(msg_dest);
            engine.write_string("%s");
            engine.write_string(safe_msg);
            engine.message_end();
        } else {
            // Fallback to direct client_print
            engine.client_print(player_index, msg_dest, payload);
        }
    }

    /// Sends a `SayText` user message to a single client.
    pub fn send_say_text(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        receiver_index: i32,
        sender_index: i32,
        message: &str,
    ) {
        let formatted = goldsrc_api::format_say_text(message);
        let payload = if !formatted.starts_with(['\x01', '\x02', '\x03', '\x04']) {
            format!("\x01{formatted}")
        } else {
            formatted
        };
        Self::send_say_text_preformatted(engine, receiver_index, sender_index, &payload);
    }

    /// Strips color escape codes (\x01..\x04) if color chat is not supported by the active game mode.
    #[inline]
    pub fn sanitize_chat_colors<'a>(
        payload: &'a str,
        allow_colors: bool,
    ) -> std::borrow::Cow<'a, str> {
        if allow_colors || !payload.bytes().any(|b| (1..=4).contains(&b)) {
            std::borrow::Cow::Borrowed(payload)
        } else {
            std::borrow::Cow::Owned(
                payload
                    .chars()
                    .filter(|&c| c != '\x01' && c != '\x02' && c != '\x03' && c != '\x04')
                    .collect(),
            )
        }
    }

    /// Sends a preformatted `SayText` payload directly to the client.
    pub fn send_say_text_preformatted(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        receiver_index: i32,
        sender_index: i32,
        payload: &str,
    ) {
        let say_text_id = engine.reg_user_msg("SayText", -1);
        if say_text_id > 0 && say_text_id < 255 {
            engine.message_begin(
                MessageDest::One as i32,
                say_text_id,
                None,
                Some(receiver_index),
            );
            // 1. Sender entity index for team color ^3 resolution
            engine.write_byte(sender_index);
            // 2. Chat message payload (sanitized if game does not support color chat)
            let allow_colors =
                crate::features::has_feature(crate::features::canon::CSTRIKE_COLOR_CHAT);
            let sanitized = Self::sanitize_chat_colors(payload, allow_colors);
            let safe_msg = if sanitized.len() > SAFE_SAYTEXT_LIMIT {
                let mut end = SAFE_SAYTEXT_LIMIT;
                while end > 0 && !sanitized.is_char_boundary(end) {
                    end -= 1;
                }
                &sanitized[..end]
            } else {
                &sanitized
            };
            engine.write_string(safe_msg);
            engine.message_end();
        } else {
            // Fallback to HUD_PRINTCHAT via ClientPrintf if SayText user message isn't registered yet
            let safe_text = format!("{payload}\n");
            engine.client_print(receiver_index, goldsrc_api::HUD_PRINTCHAT, &safe_text);
        }
    }

    /// Broadcasts a `TextMsg` to all connected clients (`MessageDest::All`).
    /// Pre-formats message once, eliminating 32 duplicate heap string allocations.
    pub fn broadcast_text_msg(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        msg_dest: i32,
        message: &str,
    ) {
        let mut payload = message.to_string();
        if (msg_dest == HUD_PRINTNOTIFY || msg_dest == HUD_PRINTCONSOLE)
            && !payload.ends_with("\n\n")
        {
            payload.push('\n');
        }
        for slot in goldsrc_api::PlayerSlot::all() {
            let idx = slot.index();
            if engine.entity_is_valid(idx) {
                Self::send_text_msg_preformatted(engine, idx, msg_dest, &payload);
            }
        }
    }

    /// Broadcasts a `SayText` message to all connected clients (`MessageDest::All`).
    /// Pre-formats chat line once, eliminating up to 64 duplicate heap allocations.
    pub fn broadcast_say_text(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        sender_index: i32,
        message: &str,
    ) {
        let formatted = goldsrc_api::format_say_text(message);
        let payload = if !formatted.starts_with(['\x01', '\x02', '\x03', '\x04']) {
            format!("\x01{formatted}")
        } else {
            formatted
        };
        for slot in goldsrc_api::PlayerSlot::all() {
            let idx = slot.index();
            if engine.entity_is_valid(idx) {
                Self::send_say_text_preformatted(engine, idx, sender_index, &payload);
            }
        }
    }

    /// Sends a `ScreenFade` user message to a specific player or broadcasts to all clients.
    pub fn send_screen_fade(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        target_player: Option<i32>,
        fade: &goldsrc_api::hud::ScreenFade,
    ) {
        if target_player.is_some_and(|idx| {
            !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&idx) || !engine.entity_is_valid(idx)
        }) {
            return;
        }

        let fade_msg_id = engine.reg_user_msg("ScreenFade", 10);
        if fade_msg_id <= 0 || fade_msg_id >= 255 {
            return;
        }

        let (dest, ent) = match target_player {
            Some(idx) => (MessageDest::One as i32, Some(idx)),
            None => (MessageDest::All as i32, None),
        };

        let duration_units = (fade.duration * 4096.0).clamp(0.0, 65535.0) as i32;
        let hold_units = (fade.hold_time * 4096.0).clamp(0.0, 65535.0) as i32;

        engine.message_begin(dest, fade_msg_id, None, ent);
        engine.write_short(duration_units);
        engine.write_short(hold_units);
        engine.write_short(fade.flags.0 as i32);
        engine.write_byte(fade.color.r as i32);
        engine.write_byte(fade.color.g as i32);
        engine.write_byte(fade.color.b as i32);
        engine.write_byte(fade.color.a as i32);
        engine.message_end();
    }

    /// Sends a `DeathMsg` user message to a specific player or broadcasts to all clients.
    pub fn send_death_msg(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        target_player: Option<i32>,
        killer_index: i32,
        victim_index: i32,
        headshot: bool,
        weapon_name: &str,
    ) {
        if target_player.is_some_and(|idx| {
            !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&idx) || !engine.entity_is_valid(idx)
        }) {
            return;
        }

        let death_msg_id = engine.reg_user_msg("DeathMsg", -1);
        if death_msg_id <= 0 || death_msg_id >= 255 {
            return;
        }

        let (dest, ent) = match target_player {
            Some(idx) => (MessageDest::One as i32, Some(idx)),
            None => (MessageDest::All as i32, None),
        };

        engine.message_begin(dest, death_msg_id, None, ent);
        engine.write_byte(killer_index);
        engine.write_byte(victim_index);
        engine.write_byte(if headshot { 1 } else { 0 });
        engine.write_string(weapon_name);
        engine.message_end();
    }

    /// Sends a `CurWeapon` user message to a specific player.
    pub fn send_cur_weapon(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        player_index: i32,
        is_active: bool,
        weapon_id: i32,
        clip_ammo: i32,
    ) {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&player_index)
            || !engine.entity_is_valid(player_index)
        {
            return;
        }

        let cur_weapon_id = engine.reg_user_msg("CurWeapon", 3);
        if cur_weapon_id <= 0 || cur_weapon_id >= 255 {
            return;
        }

        engine.message_begin(
            MessageDest::One as i32,
            cur_weapon_id,
            None,
            Some(player_index),
        );
        engine.write_byte(if is_active { 1 } else { 0 });
        engine.write_byte(weapon_id);
        engine.write_byte(clip_ammo);
        engine.message_end();
    }

    /// Sends a `Damage` user message to a specific player.
    pub fn send_damage(
        engine: &dyn goldsrc_spi::engine::EnginePort,
        player_index: i32,
        save_damage: i32,
        take_damage: i32,
        damage_bits: i32,
        origin: [f32; 3],
    ) {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&player_index)
            || !engine.entity_is_valid(player_index)
        {
            return;
        }

        let damage_id = engine.reg_user_msg("Damage", 12);
        if damage_id <= 0 || damage_id >= 255 {
            return;
        }

        engine.message_begin(MessageDest::One as i32, damage_id, None, Some(player_index));
        engine.write_byte(save_damage);
        engine.write_byte(take_damage);
        engine.write_long(damage_bits);
        engine.write_coord(origin[0]);
        engine.write_coord(origin[1]);
        engine.write_coord(origin[2]);
        engine.message_end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goldsrc_api::cvar::{CvarFlags, CvarPort};
    use goldsrc_api::entity::EntitySpawner;
    use goldsrc_spi::engine::{
        EngineConsole, EngineEntities, EngineExtensions, EngineMessages, EnginePhysics,
        EnginePrecache, EngineSound, TraceResult,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockNetEngine {
        messages: Mutex<Vec<(i32, i32, Option<i32>)>>,
        bytes: Mutex<Vec<i32>>,
        shorts: Mutex<Vec<i32>>,
        longs: Mutex<Vec<i32>>,
        coords: Mutex<Vec<f32>>,
        strings: Mutex<Vec<String>>,
        ended: Mutex<usize>,
    }

    impl EngineExtensions for MockNetEngine {}

    impl EnginePrecache for MockNetEngine {
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

    impl EngineMessages for MockNetEngine {
        fn message_begin(&self, d: i32, t: i32, _o: Option<[f32; 3]>, e: Option<i32>) {
            self.messages.lock().unwrap().push((d, t, e));
        }
        fn message_end(&self) {
            *self.ended.lock().unwrap() += 1;
        }
        fn write_byte(&self, b: i32) {
            self.bytes.lock().unwrap().push(b);
        }
        fn write_char(&self, _c: i32) {}
        fn write_short(&self, s: i32) {
            self.shorts.lock().unwrap().push(s);
        }
        fn write_long(&self, l: i32) {
            self.longs.lock().unwrap().push(l);
        }
        fn write_angle(&self, _a: f32) {}
        fn write_coord(&self, c: f32) {
            self.coords.lock().unwrap().push(c);
        }
        fn write_string(&self, s: &str) {
            self.strings.lock().unwrap().push(s.to_string());
        }
        fn write_entity(&self, _e: i32) {}
        fn reg_user_msg(&self, name: &str, _size: i32) -> i32 {
            match name {
                "TextMsg" => 64,
                "SayText" => 65,
                "ScreenFade" => 66,
                "DeathMsg" => 67,
                "CurWeapon" => 68,
                "Damage" => 69,
                _ => -1,
            }
        }
    }

    impl EntitySpawner for MockNetEngine {
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

    impl EngineEntities for MockNetEngine {
        fn entity_is_valid(&self, index: i32) -> bool {
            goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index)
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
            Some("Player".into())
        }
        fn player_team(&self, _index: i32) -> i32 {
            1
        }
        fn player_lang(&self, _index: i32) -> Option<String> {
            Some("en".into())
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

    impl CvarPort for MockNetEngine {
        fn cvar_get_string(&self, _n: &str) -> Option<String> {
            None
        }
        fn cvar_set_string(&self, _n: &str, _v: &str) {}
        fn cvar_get_float(&self, _n: &str) -> f32 {
            0.0
        }
        fn cvar_set_float(&self, _n: &str, _v: f32) {}
        fn cvar_register(&self, _name: &str, _default_value: &str, _flags: CvarFlags) -> bool {
            true
        }
    }

    impl EngineConsole for MockNetEngine {
        fn server_command(&self, _cmd: &str) {}
        fn server_print(&self, _msg: &str) {}
        fn client_print(&self, _client_index: i32, _dest: i32, _message: &str) {}
    }

    impl EngineSound for MockNetEngine {
        fn emit_sound(
            &self,
            _entity: i32,
            _channel: i32,
            _sample: &str,
            _volume: f32,
            _attenuation: f32,
            _flags: i32,
            _pitch: i32,
        ) {
        }
        fn emit_ambient_sound(
            &self,
            _entity: i32,
            _pos: [f32; 3],
            _sample: &str,
            _volume: f32,
            _attenuation: f32,
            _flags: i32,
            _pitch: i32,
        ) {
        }
    }

    impl EnginePhysics for MockNetEngine {
        fn trace_line(
            &self,
            _start: [f32; 3],
            _end: [f32; 3],
            _flags: i32,
            _skip_entity: i32,
        ) -> TraceResult {
            TraceResult {
                all_solid: false,
                start_solid: false,
                in_open: true,
                in_water: false,
                fraction: 1.0,
                end_pos: [0.0; 3],
                plane_normal: [0.0; 3],
                hit_entity: -1,
            }
        }
        fn trace_hull(
            &self,
            _start: [f32; 3],
            _end: [f32; 3],
            _flags: i32,
            _hull_number: i32,
            _skip_entity: i32,
        ) -> TraceResult {
            TraceResult {
                all_solid: false,
                start_solid: false,
                in_open: true,
                in_water: false,
                fraction: 1.0,
                end_pos: [0.0; 3],
                plane_normal: [0.0; 3],
                hit_entity: -1,
            }
        }
        fn point_contents(&self, _point: [f32; 3]) -> i32 {
            0
        }
    }

    #[test]
    fn test_dispatcher_send_text_msg() {
        let engine = MockNetEngine::default();
        NetworkMessageDispatcher::send_text_msg(&engine, 1, HUD_PRINTCENTER, "Round Started!");

        let msgs = engine.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], (MessageDest::One as i32, 64, Some(1)));

        let bytes = engine.bytes.lock().unwrap();
        assert_eq!(bytes[0], HUD_PRINTCENTER);

        let strings = engine.strings.lock().unwrap();
        assert_eq!(strings[0], "%s");
        assert_eq!(strings[1], "Round Started!");
        assert_eq!(*engine.ended.lock().unwrap(), 1);
    }

    #[test]
    fn test_dispatcher_send_say_text() {
        let engine = MockNetEngine::default();
        // 1. Without CSTRIKE_COLOR_CHAT, colors are sanitized for vanilla GoldSrc
        NetworkMessageDispatcher::send_say_text(&engine, 2, 1, "Hello from team!");

        let msgs = engine.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], (MessageDest::One as i32, 65, Some(2)));

        let bytes = engine.bytes.lock().unwrap();
        assert_eq!(bytes[0], 1); // sender index

        let strings = engine.strings.lock().unwrap();
        assert_eq!(strings[0], "Hello from team!");
        assert_eq!(*engine.ended.lock().unwrap(), 1);
        drop(msgs);
        drop(bytes);
        drop(strings);

        // 2. With CSTRIKE_COLOR_CHAT registered, color codes are preserved
        crate::features::register_feature(crate::features::canon::CSTRIKE_COLOR_CHAT);
        let engine2 = MockNetEngine::default();
        NetworkMessageDispatcher::send_say_text(&engine2, 2, 1, "Hello from team!");
        let strings2 = engine2.strings.lock().unwrap();
        assert_eq!(strings2[0], "\x01Hello from team!");
        crate::features::unregister_feature(crate::features::canon::CSTRIKE_COLOR_CHAT);
    }

    #[test]
    fn test_dispatcher_player_print() {
        let engine = MockNetEngine::default();
        NetworkMessageDispatcher::dispatch_player_print(
            &engine,
            3,
            PrintTarget::Notify,
            "Notice message",
        );
        let msgs = engine.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], (MessageDest::One as i32, 64, Some(3)));
        assert_eq!(*engine.ended.lock().unwrap(), 1);
    }

    #[test]
    fn test_dispatcher_send_screen_fade() {
        let engine = MockNetEngine::default();
        let fade = goldsrc_api::hud::ScreenFade::damage_flash();
        NetworkMessageDispatcher::send_screen_fade(&engine, Some(5), &fade);

        let msgs = engine.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], (MessageDest::One as i32, 66, Some(5)));

        let shorts = engine.shorts.lock().unwrap();
        assert_eq!(shorts.len(), 3);
        assert_eq!(shorts[0], (0.2 * 4096.0) as i32);
        assert_eq!(shorts[1], (0.1 * 4096.0) as i32);
        assert_eq!(shorts[2], fade.flags.0 as i32);

        let bytes = engine.bytes.lock().unwrap();
        assert_eq!(bytes.len(), 4);
        assert_eq!(bytes[0], fade.color.r as i32);
        assert_eq!(bytes[1], fade.color.g as i32);
        assert_eq!(bytes[2], fade.color.b as i32);
        assert_eq!(bytes[3], fade.color.a as i32);
        assert_eq!(*engine.ended.lock().unwrap(), 1);
    }

    #[test]
    fn test_dispatcher_send_death_msg() {
        let engine = MockNetEngine::default();
        NetworkMessageDispatcher::send_death_msg(&engine, None, 1, 2, true, "deagle");

        let msgs = engine.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], (MessageDest::All as i32, 67, None));

        let bytes = engine.bytes.lock().unwrap();
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes[0], 1);
        assert_eq!(bytes[1], 2);
        assert_eq!(bytes[2], 1); // headshot

        let strings = engine.strings.lock().unwrap();
        assert_eq!(strings.len(), 1);
        assert_eq!(strings[0], "deagle");
        assert_eq!(*engine.ended.lock().unwrap(), 1);
    }

    #[test]
    fn test_dispatcher_send_cur_weapon() {
        let engine = MockNetEngine::default();
        NetworkMessageDispatcher::send_cur_weapon(&engine, 4, true, 28, 30);

        let msgs = engine.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], (MessageDest::One as i32, 68, Some(4)));

        let bytes = engine.bytes.lock().unwrap();
        assert_eq!(bytes.len(), 3);
        assert_eq!(bytes[0], 1); // active
        assert_eq!(bytes[1], 28); // weapon_id
        assert_eq!(bytes[2], 30); // clip_ammo
        assert_eq!(*engine.ended.lock().unwrap(), 1);
    }

    #[test]
    fn test_dispatcher_send_damage() {
        let engine = MockNetEngine::default();
        NetworkMessageDispatcher::send_damage(
            &engine,
            7,
            15,
            35,
            1 << 1, // DMG_BULLET
            [100.0, -200.0, 50.0],
        );

        let msgs = engine.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], (MessageDest::One as i32, 69, Some(7)));

        let bytes = engine.bytes.lock().unwrap();
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes[0], 15);
        assert_eq!(bytes[1], 35);

        let longs = engine.longs.lock().unwrap();
        assert_eq!(longs.len(), 1);
        assert_eq!(longs[0], 1 << 1);

        let coords = engine.coords.lock().unwrap();
        assert_eq!(coords.len(), 3);
        assert_eq!(coords[0], 100.0);
        assert_eq!(coords[1], -200.0);
        assert_eq!(coords[2], 50.0);
        assert_eq!(*engine.ended.lock().unwrap(), 1);
    }
}
