//! Wasmtime HostState and api::Host implementation.
//!
//! Provides the host-function environment exposed to WASM plugins:
//! entity manipulation, cvars, networking messages, HUD, capabilities, and storage sandbox.

use crate::bindings::goldsrc::engine::api;
use goldsrc_spi::engine::EnginePort as GoldsrcEngine;
use std::sync::Arc;

/// Wasmtime store state exposed to WASM plugins via host functions.
pub struct HostState {
    /// Engine bridge for real game-state access.
    pub engine: Arc<dyn GoldsrcEngine>,
    /// Per-store memory and table limit enforcement.
    pub limits: wasmtime::StoreLimits,
    /// Identifier of the calling plugin.
    pub plugin_name: String,
    /// Explicitly granted sandbox permissions from metadata.
    pub permissions: Vec<String>,
    /// Explicitly allowed shared storage buckets from metadata.
    pub shared_buckets: Vec<String>,
}

impl HostState {
    /// Checks if the calling plugin has been granted a specific permission.
    /// If no explicit `permissions` are declared (empty vector), backward-compatible legacy access is permitted with a debug trace.
    /// If `permissions` is non-empty, checks for explicit match or wildcard (e.g. `cvar:*`, `*`).
    pub fn check_permission(&self, perm: &str) -> bool {
        if self.permissions.is_empty() {
            return true;
        }

        let has_perm = self.permissions.iter().any(|p| {
            p == "*" || p == perm || (p.ends_with(":*") && perm.starts_with(&p[..p.len() - 1]))
        });

        if !has_perm {
            let err = crate::error::SecurityError::PermissionDenied {
                plugin: self.plugin_name.clone(),
                permission: perm.to_string(),
                declared: self.permissions.clone(),
            };
            log::warn!(target: goldsrc_api::consts::log_targets::AUTH, "{err}");
            crate::host_log(&format!("[Security Warning] {err}"));
        }
        has_perm
    }

    /// Resolves bucket name enforcing plugin isolation and allowlist sharing.
    pub fn resolve_bucket(&self, bucket: &str) -> Option<String> {
        if bucket.contains('/') {
            // Check if bucket is explicitly allowlisted
            if self.shared_buckets.iter().any(|b| b == bucket) {
                Some(bucket.to_string())
            } else {
                let err = crate::error::SecurityError::UnauthorizedBucketAccess {
                    plugin: self.plugin_name.clone(),
                    bucket: bucket.to_string(),
                };
                log::error!(target: goldsrc_api::consts::log_targets::STORAGE, "{err}");
                crate::host_log(&format!("[Security Error] {err}"));
                None
            }
        } else {
            // Auto-prefix with plugin name
            Some(format!("{}/{}", self.plugin_name, bucket))
        }
    }
}

impl api::Host for HostState {
    fn host_log(&mut self, msg: String) {
        crate::host_log(&msg);
    }

    fn host_time(&mut self) -> f32 {
        crate::get_host_time()
    }

    fn host_entity_is_valid(&mut self, index: i32) -> bool {
        self.engine.entity_is_valid(index)
    }
    fn host_entity_classname(&mut self, index: i32) -> Option<String> {
        self.engine.entity_classname(index)
    }
    fn host_entity_health(&mut self, index: i32) -> f32 {
        self.engine.entity_health(index)
    }
    fn host_entity_set_health(&mut self, index: i32, health: f32) {
        self.engine.entity_set_health(index, health);
    }
    fn host_entity_origin(&mut self, index: i32) -> api::Vector3 {
        let [x, y, z] = self.engine.entity_origin(index);
        api::Vector3 { x, y, z }
    }
    fn host_entity_set_origin(&mut self, index: i32, pos: api::Vector3) {
        self.engine.entity_set_origin(index, [pos.x, pos.y, pos.z]);
    }
    fn host_entity_velocity(&mut self, index: i32) -> api::Vector3 {
        let [x, y, z] = self.engine.entity_velocity(index);
        api::Vector3 { x, y, z }
    }
    fn host_entity_set_velocity(&mut self, index: i32, vel: api::Vector3) {
        self.engine
            .entity_set_velocity(index, [vel.x, vel.y, vel.z]);
    }
    fn host_entity_angles(&mut self, index: i32) -> api::Vector3 {
        let [x, y, z] = self.engine.entity_angles(index);
        api::Vector3 { x, y, z }
    }
    fn host_entity_set_angles(&mut self, index: i32, angles: api::Vector3) {
        self.engine
            .entity_set_angles(index, [angles.x, angles.y, angles.z]);
    }
    fn host_create_named_entity(&mut self, classname: String) -> Option<i32> {
        if !self.check_permission(goldsrc_api::consts::permissions::ENTITY_CREATE) {
            return None;
        }
        self.engine.create_named_entity(&classname)
    }
    fn host_remove_entity(&mut self, index: i32) {
        if !self.check_permission(goldsrc_api::consts::permissions::ENTITY_REMOVE) {
            return;
        }
        self.engine.remove_entity(index);
    }
    fn host_drop_to_floor(&mut self, index: i32) -> i32 {
        self.engine.drop_to_floor(index)
    }
    fn host_entity_key_value(&mut self, index: i32, key: String, value: String) -> bool {
        self.engine.entity_key_value(index, &key, &value)
    }

    fn host_player_name(&mut self, index: i32) -> Option<String> {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return None;
        }
        self.engine.player_name(index)
    }
    fn host_player_lang(&mut self, index: i32) -> Option<String> {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return None;
        }
        self.engine.player_lang(index)
    }
    fn host_player_team(&mut self, index: i32) -> i32 {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return 0;
        }
        self.engine.player_team(index)
    }
    fn host_player_armorvalue(&mut self, index: i32) -> f32 {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return 0.0;
        }
        self.engine.player_armorvalue(index)
    }
    fn host_player_set_armorvalue(&mut self, index: i32, armor: f32) {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return;
        }
        self.engine.player_set_armorvalue(index, armor);
    }

    fn host_player_auth_id(&mut self, index: i32) -> Option<String> {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return None;
        }
        self.engine.player_auth_id(index)
    }

    fn host_player_ip(&mut self, index: i32) -> Option<String> {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return None;
        }
        self.engine.player_ip(index)
    }

    fn host_player_user_id(&mut self, index: i32) -> i32 {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return 0;
        }
        self.engine.player_user_id(index) as i32
    }

    fn host_player_set_listening(&mut self, receiver: i32, sender: i32, listen: bool) -> bool {
        self.engine.set_client_listening(receiver, sender, listen)
    }

    fn host_player_set_maxspeed(&mut self, index: i32, speed: f32) {
        self.engine.set_player_maxspeed(index, speed);
    }

    fn host_server_command(&mut self, command: String) {
        if !self.check_permission(goldsrc_api::consts::permissions::ENGINE_SERVER_COMMAND) {
            return;
        }
        self.engine.server_command(&command);
    }

    fn host_client_command(&mut self, player_index: i32, command: String) {
        if !self.check_permission(goldsrc_api::consts::permissions::ENGINE_CLIENT_COMMAND) {
            return;
        }
        self.engine.client_command(player_index, &command);
    }

    fn host_config_exec(&mut self, preset_path: String) -> Result<bool, String> {
        if !self.check_permission(goldsrc_api::consts::permissions::CONFIG_EXEC) {
            return Err("Permission denied: config:exec required".to_string());
        }
        // Validate path and execute preset
        let path = std::path::Path::new(&preset_path);
        if !path.exists() {
            return Err(format!("Preset file not found: {}", preset_path));
        }
        // Fallback execution via engine command if standard .cfg, or success for validated path
        self.engine
            .server_command(&format!("exec \"{}\"\n", preset_path));
        Ok(true)
    }

    fn host_cvar_get_float(&mut self, name: String) -> f32 {
        self.engine.cvar_get_float(&name)
    }
    fn host_cvar_set_float(&mut self, name: String, val: f32) {
        if !self.check_permission(goldsrc_api::consts::permissions::CVAR_SET) {
            return;
        }
        self.engine.cvar_set_float(&name, val);
    }
    fn host_cvar_get_string(&mut self, name: String) -> Option<String> {
        self.engine.cvar_get_string(&name)
    }
    fn host_cvar_set_string(&mut self, name: String, val: String) {
        if !self.check_permission(goldsrc_api::consts::permissions::CVAR_SET) {
            return;
        }
        self.engine.cvar_set_string(&name, &val);
    }

    fn host_precache_model(&mut self, path: String) -> i32 {
        self.engine.precache_model(&path)
    }
    fn host_precache_sound(&mut self, path: String) -> i32 {
        self.engine.precache_sound(&path)
    }
    fn host_precache_generic(&mut self, path: String) -> i32 {
        self.engine.precache_generic(&path)
    }

    fn host_emit_sound(
        &mut self,
        entity: i32,
        channel: i32,
        sample: String,
        volume: f32,
        attenuation: f32,
        sound_flags: i32,
        pitch: i32,
    ) {
        self.engine.emit_sound(
            entity,
            channel,
            &sample,
            volume,
            attenuation,
            sound_flags,
            pitch,
        );
    }

    fn host_print_chat(&mut self, player_index: i32, message: String) {
        if player_index < 0 {
            self.engine.server_print(&format!("[Chat] {message}\n"));
            return;
        }

        if player_index != 0
            && (!goldsrc_api::PlayerSlot::VALID_RANGE.contains(&player_index)
                || !self.engine.entity_is_valid(player_index))
        {
            self.engine
                .server_print(&format!("[Chat to #{player_index}] {message}\n"));
            return;
        }

        let expanded = crate::format_message_placeholders(player_index, &message);
        let chunks = goldsrc_api::chat::split_chat_chunks(&expanded);
        let chunks = if chunks.is_empty() {
            vec![expanded]
        } else {
            chunks
        };

        let dest = if player_index == 0 {
            goldsrc_spi::engine::MessageDest::All as i32
        } else {
            goldsrc_spi::engine::MessageDest::One as i32
        };
        let target_edict = if player_index == 0 {
            None
        } else {
            Some(player_index)
        };
        let sender_id = if player_index == 0 { 1 } else { player_index };

        for chunk in chunks {
            let formatted = goldsrc_api::format_say_text(&chunk);
            let say_text_id = self.engine.reg_user_msg("SayText", -1);
            let msg_id = if say_text_id <= 0 { 76 } else { say_text_id };
            self.engine.message_begin(dest, msg_id, None, target_edict);
            // In GoldSrc CS 1.6 SayText, first byte is the sender entity index (1..32 for player colors, or 0)
            self.engine.write_byte(sender_id);

            // CS 1.6 client requires leading color byte \x01 if no color prefix is present
            let payload = if !formatted.starts_with(['\x01', '\x02', '\x03', '\x04']) {
                format!("\x01{formatted}")
            } else {
                formatted
            };

            // Truncate message if oversized to prevent buffer overflow (SayText payload max 192 bytes)
            let safe_msg = if payload.len() > goldsrc_api::consts::SAFE_SAYTEXT_LIMIT {
                let mut end = goldsrc_api::consts::SAFE_SAYTEXT_LIMIT;
                while end > 0 && !payload.is_char_boundary(end) {
                    end -= 1;
                }
                &payload[..end]
            } else {
                &payload
            };
            // SayText string must be sent without extra trailing newline
            self.engine.write_string(safe_msg);
            self.engine.message_end();
        }
    }

    fn host_print_center(&mut self, player_index: i32, message: String) {
        let expanded = crate::format_message_placeholders(player_index, &message);
        if player_index < 0 {
            self.engine.server_print(&format!("[Center] {expanded}\n"));
            return;
        }

        let formatted = goldsrc_api::format_center_text(&expanded);
        let text_msg_id = self.engine.reg_user_msg("TextMsg", -1);
        let msg_id = if text_msg_id <= 0 { 75 } else { text_msg_id };

        let dest = if player_index == 0 {
            goldsrc_spi::engine::MessageDest::All as i32
        } else {
            if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&player_index)
                || !self.engine.entity_is_valid(player_index)
            {
                return;
            }
            goldsrc_spi::engine::MessageDest::One as i32
        };

        let target_edict = if player_index == 0 {
            None
        } else {
            Some(player_index)
        };

        self.engine.message_begin(dest, msg_id, None, target_edict);

        // AMX Mod X / HLSDK UTIL_ClientPrint protocol for center messages:
        // 1. Write destination byte: HUD_PRINTCENTER (4)
        // 2. Write format string: "%s"
        // 3. Write formatted message (newlines replaced with '\r', safe truncated to <= 185 bytes)
        self.engine.write_byte(goldsrc_api::HUD_PRINTCENTER);
        self.engine.write_string("%s");

        let safe_msg = if formatted.len() > 185 {
            let mut end = 185;
            while end > 0 && !formatted.is_char_boundary(end) {
                end -= 1;
            }
            &formatted[..end]
        } else {
            &formatted
        };

        self.engine.write_string(safe_msg);
        self.engine.message_end();
    }

    fn host_print_console(&mut self, player_index: i32, message: String) {
        if player_index <= 0 || !self.engine.entity_is_valid(player_index) {
            let formatted = if message.ends_with('\n') {
                message
            } else {
                format!("{message}\n")
            };
            self.engine.server_print(&formatted);
            return;
        }
        let formatted = if message.ends_with('\n') {
            message
        } else {
            format!("{message}\n")
        };
        let text_msg_id = self.engine.reg_user_msg("TextMsg", -1);
        if text_msg_id > 0 && text_msg_id < 255 {
            let mut payload = formatted;
            if !payload.ends_with("\n\n") {
                payload.push('\n');
            }
            let safe_msg = if payload.len() > 185 {
                let mut end = 185;
                while end > 0 && !payload.is_char_boundary(end) {
                    end -= 1;
                }
                &payload[..end]
            } else {
                &payload
            };
            self.engine.message_begin(
                goldsrc_spi::engine::MessageDest::One as i32,
                text_msg_id,
                None,
                Some(player_index),
            );
            self.engine.write_byte(goldsrc_api::HUD_PRINTCONSOLE);
            self.engine.write_string("%s");
            self.engine.write_string(safe_msg);
            self.engine.message_end();
        } else {
            self.engine
                .client_print(player_index, goldsrc_api::HUD_PRINTCONSOLE, &formatted);
        }
    }

    fn host_print_notify(&mut self, player_index: i32, message: String) {
        if player_index <= 0 || !self.engine.entity_is_valid(player_index) {
            self.engine
                .server_print(&format!("[Notify#{player_index}] {message}\n"));
            return;
        }
        let formatted = goldsrc_api::format_notify_text(&message);
        let text_msg_id = self.engine.reg_user_msg("TextMsg", -1);
        if text_msg_id > 0 && text_msg_id < 255 {
            let mut payload = formatted;
            if !payload.ends_with("\n\n") {
                payload.push('\n');
            }
            let safe_msg = if payload.len() > 185 {
                let mut end = 185;
                while end > 0 && !payload.is_char_boundary(end) {
                    end -= 1;
                }
                &payload[..end]
            } else {
                &payload
            };
            self.engine.message_begin(
                goldsrc_spi::engine::MessageDest::One as i32,
                text_msg_id,
                None,
                Some(player_index),
            );
            self.engine.write_byte(goldsrc_api::HUD_PRINTNOTIFY);
            self.engine.write_string("%s");
            self.engine.write_string(safe_msg);
            self.engine.message_end();
        } else {
            self.engine
                .client_print(player_index, goldsrc_api::HUD_PRINTNOTIFY, &formatted);
        }
    }

    fn host_dispatch_spawn(&mut self, index: i32) -> i32 {
        self.engine.dispatch_spawn(index)
    }

    fn host_dispatch_touch(&mut self, touched: i32, other: i32) {
        self.engine.dispatch_touch(touched, other);
    }

    fn host_show_menu(&mut self, player_index: i32, keys_mask: i32, timeout: i32, text: String) {
        if player_index <= 0 {
            return;
        }
        let show_menu_id = self.engine.reg_user_msg("ShowMenu", -1);
        if show_menu_id <= 0 || show_menu_id == 255 {
            return;
        }

        crate::notify_show_menu(player_index, keys_mask, timeout, &text);

        if keys_mask == 0 {
            crate::clear_active_menu_owner(player_index);
        } else {
            crate::set_active_menu_owner(player_index, self.plugin_name.clone());
        }

        if text.is_empty() {
            self.engine.message_begin(
                goldsrc_spi::engine::MessageDest::One as i32,
                show_menu_id,
                None,
                Some(player_index),
            );
            self.engine.write_short(keys_mask);
            self.engine.write_char(timeout);
            self.engine.write_byte(0);
            self.engine.write_string("");
            self.engine.message_end();
            return;
        }

        let max_chunk = goldsrc_api::consts::MAX_SHOW_MENU_CHUNK_SIZE;
        let mut remaining = &text[..];

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

            self.engine.message_begin(
                goldsrc_spi::engine::MessageDest::One as i32,
                show_menu_id,
                None,
                Some(player_index),
            );
            self.engine.write_short(keys_mask);
            self.engine.write_char(timeout);
            self.engine.write_byte(if has_more { 1 } else { 0 });
            self.engine.write_string(chunk);
            self.engine.message_end();
        }
    }

    fn host_send_hud_message(
        &mut self,
        _player_index: i32,
        channel: i32,
        x: f32,
        y: f32,
        r: i32,
        g: i32,
        b: i32,
        a: i32,
        effect: i32,
        fade_in: f32,
        fade_out: f32,
        hold_time: f32,
        text: String,
    ) {
        let x_val = (if x < 0.0 { -1.0 } else { x } * 8192.0) as i32;
        let y_val = (if y < 0.0 { -1.0 } else { y } * 8192.0) as i32;

        self.engine.message_begin(
            goldsrc_spi::engine::MessageDest::Broadcast as i32,
            goldsrc_api::consts::SVC_TEMPENTITY,
            None,
            None,
        );
        self.engine
            .write_byte(goldsrc_api::consts::TE_TEXTMESSAGE as i32);
        self.engine.write_byte(channel.clamp(1, 4));
        self.engine.write_short(x_val);
        self.engine.write_short(y_val);
        self.engine.write_byte(effect.clamp(0, 2));
        self.engine.write_byte(r.clamp(0, 255));
        self.engine.write_byte(g.clamp(0, 255));
        self.engine.write_byte(b.clamp(0, 255));
        self.engine.write_byte(a.clamp(0, 255));
        self.engine.write_byte(r.clamp(0, 255)); // 2nd color fallback
        self.engine.write_byte(g.clamp(0, 255));
        self.engine.write_byte(b.clamp(0, 255));
        self.engine.write_byte(a.clamp(0, 255));
        self.engine.write_short((fade_in * 256.0) as i32);
        self.engine.write_short((fade_out * 256.0) as i32);
        self.engine.write_short((hold_time * 256.0) as i32);
        if effect.clamp(0, 2) == 2 {
            self.engine.write_short(0); // fx_time placeholder
        }
        self.engine.write_string(&text);
        self.engine.message_end();
    }

    fn host_send_dhud_message(
        &mut self,
        player_index: i32,
        x: f32,
        y: f32,
        r: i32,
        g: i32,
        b: i32,
        _a: i32,
        effect: i32,
        fade_in: f32,
        fade_out: f32,
        hold_time: f32,
        text: String,
    ) {
        const SVC_DIRECTOR: i32 = 51;
        const DRC_CMD_MESSAGE: i32 = 6;

        let (dest, target_idx) = if player_index <= 0 {
            (goldsrc_spi::engine::MessageDest::Broadcast as i32, None)
        } else {
            (
                goldsrc_spi::engine::MessageDest::One as i32,
                Some(player_index),
            )
        };

        let mut len = text.len().min(128);
        while len > 0 && !text.is_char_boundary(len) {
            len -= 1;
        }
        let safe_text = &text[..len];

        // Pack color into 0x00RRGGBB format expected by client VGUI director parser
        let packed_color = b.clamp(0, 255) | (g.clamp(0, 255) << 8) | (r.clamp(0, 255) << 16);

        self.engine
            .message_begin(dest, SVC_DIRECTOR, None, target_idx);
        self.engine.write_byte((len as i32) + 31);
        self.engine.write_byte(DRC_CMD_MESSAGE);
        self.engine.write_byte(effect.clamp(0, 2));
        self.engine.write_long(packed_color);
        self.engine.write_long(x.to_bits() as i32);
        self.engine.write_long(y.to_bits() as i32);
        self.engine.write_long(fade_in.to_bits() as i32);
        self.engine.write_long(fade_out.to_bits() as i32);
        self.engine.write_long(hold_time.to_bits() as i32);
        self.engine.write_long(0); // fx_time
        self.engine.write_string(safe_text);
        self.engine.message_end();
    }

    fn host_register_capability(&mut self, name: String, description: String) -> bool {
        if name.is_empty() || name.len() > 256 || description.len() > 4096 {
            return false;
        }
        goldsrc_api::auth::Auth::register_capability(&name, &description)
    }

    fn host_has_capability(&mut self, player_index: i32, name: String) -> bool {
        goldsrc_api::auth::Auth::has_capability(player_index, &name)
    }

    fn host_grant_capability(&mut self, player_index: i32, name: String) -> bool {
        goldsrc_api::auth::Auth::grant_capability(player_index, &name)
    }

    fn host_revoke_capability(&mut self, player_index: i32, name: String) -> bool {
        goldsrc_api::auth::Auth::revoke_capability(player_index, &name)
    }

    fn host_storage_get(&mut self, bucket: String, key: String) -> Option<Vec<u8>> {
        let resolved = self.resolve_bucket(&bucket)?;
        if let Ok(lock) = crate::STORAGE_GET_CB.read() {
            if let Some(cb) = *lock {
                return cb(&resolved, &key);
            }
        }
        None
    }

    fn host_storage_set(&mut self, bucket: String, key: String, val: Vec<u8>) -> bool {
        let Some(resolved) = self.resolve_bucket(&bucket) else {
            return false;
        };
        if let Ok(lock) = crate::STORAGE_SET_CB.read() {
            if let Some(cb) = *lock {
                return cb(&resolved, &key, &val);
            }
        }
        false
    }

    fn host_storage_delete(&mut self, bucket: String, key: String) -> bool {
        let Some(resolved) = self.resolve_bucket(&bucket) else {
            return false;
        };
        if let Ok(lock) = crate::STORAGE_DELETE_CB.read() {
            if let Some(cb) = *lock {
                return cb(&resolved, &key);
            }
        }
        false
    }

    fn host_storage_fetch_add(&mut self, bucket: String, key: String, delta: i64) -> i64 {
        let Some(resolved) = self.resolve_bucket(&bucket) else {
            return 0;
        };
        if let Ok(lock) = crate::STORAGE_FETCH_ADD_CB.read() {
            if let Some(cb) = *lock {
                return cb(&resolved, &key, delta);
            }
        }
        0
    }

    fn host_translate(&mut self, dict: String, lang: String, key: String) -> String {
        if let Ok(lock) = crate::TRANSLATE_CB.read() {
            if let Some(cb) = *lock {
                return cb(&self.plugin_name, &dict, &lang, &key);
            }
        }
        key
    }

    fn host_register_placeholder(&mut self, name: String, _description: String) -> bool {
        crate::manager::register_host_placeholder(&name, &self.plugin_name);
        true
    }

    fn host_has_feature(&mut self, token: u64) -> bool {
        if let Ok(lock) = crate::FEATURE_QUERY_CB.read()
            && let Some(cb) = *lock
        {
            return cb(token);
        }
        self.engine.has_feature(token)
    }

    fn host_is_extension_available(&mut self, name: String, version_req: Option<String>) -> bool {
        self.engine
            .is_extension_available(&name, version_req.as_deref())
    }

    fn host_get_extension_version(&mut self, name: String) -> Option<String> {
        self.engine.get_extension_version(&name)
    }

    fn host_fs_read_text(&mut self, path: String) -> Result<String, String> {
        if let Ok(lock) = crate::VFS_READ_TEXT_CB.read() {
            if let Some(cb) = *lock {
                return cb(&self.plugin_name, &path);
            }
        }
        Err("Host VFS not configured".to_string())
    }

    fn host_fs_read_bytes(&mut self, path: String) -> Result<Vec<u8>, String> {
        if let Ok(lock) = crate::VFS_READ_BYTES_CB.read() {
            if let Some(cb) = *lock {
                return cb(&self.plugin_name, &path);
            }
        }
        Err("Host VFS not configured".to_string())
    }

    fn host_fs_list_dir(
        &mut self,
        path: String,
    ) -> Result<Vec<crate::bindings::goldsrc::engine::api::DirEntry>, String> {
        if let Ok(lock) = crate::VFS_LIST_DIR_CB.read() {
            if let Some(cb) = *lock {
                let entries = cb(&self.plugin_name, &path)?;
                return Ok(entries
                    .into_iter()
                    .map(
                        |(name, is_dir, size)| crate::bindings::goldsrc::engine::api::DirEntry {
                            name,
                            is_dir,
                            size,
                        },
                    )
                    .collect());
            }
        }
        Err("Host VFS not configured".to_string())
    }
}
