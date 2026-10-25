//! Network serialization and dispatching for HUD and DHUD screen messages.

use goldsrc_api::hud::{HudEffect, HudKind, HudMessage};
use goldsrc_spi::engine::EnginePort;

/// Serializes and sends a `HudMessage` to a target player (or all players if player_idx is None).
pub fn send_hud_message(engine: &dyn EnginePort, player_idx: Option<i32>, msg: &HudMessage) {
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
            // Calculate fixed-point coordinates (-1.0 maps to -1.0)
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

            // SVC_TEMPENTITY -> TE_TEXTMESSAGE
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
            // fx_time is only present in the TE_TEXTMESSAGE wire format when effect == 2 (typewriter)
            if effect_val == 2 {
                engine.write_short(fx_time_val);
            }
            engine.write_string(&msg.text);
            engine.message_end();
        }
        HudKind::Dhud => {
            // True Director HUD (DHUD) wire format:
            // SVC_DIRECTOR (51) -> length + 31 -> DRC_CMD_MESSAGE (6) -> textparms -> text
            const SVC_DIRECTOR: i32 = 51;
            const DRC_CMD_MESSAGE: i32 = 6;

            let mut len = msg.text.len().min(128);
            while len > 0 && !msg.text.is_char_boundary(len) {
                len -= 1;
            }
            let safe_text = &msg.text[..len];

            // Pack color into 0x00RRGGBB format
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

#[cfg(test)]
mod tests {
    #[test]
    fn test_hud_utf8_char_boundary_truncation() {
        // Construct Russian Cyrillic string where byte 128 falls in the middle of a 2-byte character
        let russian_text = "Тестовое сообщение для проверки корректности обрезки многобайтовых символов UTF-8 в Director HUD";
        assert!(russian_text.len() > 128);

        let mut len = russian_text.len().min(128);
        while len > 0 && !russian_text.is_char_boundary(len) {
            len -= 1;
        }
        let safe = &russian_text[..len];
        assert!(safe.len() <= 128);
        assert!(std::str::from_utf8(safe.as_bytes()).is_ok());
    }
}
