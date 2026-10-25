use super::EngineBackend;
use crate::{call_engfunc, call_engfunc_ret};
use goldsrc_api::consts::FL_CLIENT;
use goldsrc_api::consts::log_targets::CORE;
use goldsrc_api::cvar::CvarPort;
use goldsrc_api::entity::EntitySpawner;
use goldsrc_api::{Angles, Armor, Health, Origin, Velocity};
use goldsrc_spi::engine::{EngineEntities, EngineMessages, MessageDest};
use goldsrc_spi::identity::{AuthState, AuthSubject, PlayerIdentity, SteamId};
use goldsrc_sys::{KeyValueData, edict_t, ffi};

pub type GamedllSpawnFn = unsafe extern "C" fn(*mut edict_t) -> i32;
pub type GamedllTouchFn = unsafe extern "C" fn(*mut edict_t, *mut edict_t);
pub type GamedllKeyValueFn = unsafe extern "C" fn(*mut edict_t, *mut KeyValueData);

static GAME_DLL_SPAWN: std::sync::OnceLock<GamedllSpawnFn> = std::sync::OnceLock::new();
static GAME_DLL_TOUCH: std::sync::OnceLock<GamedllTouchFn> = std::sync::OnceLock::new();
static GAME_DLL_KEY_VALUE: std::sync::OnceLock<GamedllKeyValueFn> = std::sync::OnceLock::new();

/// Registers the real GameDLL `DispatchSpawn` (call once after DLL load).
pub fn set_game_dll_spawn(f: GamedllSpawnFn) {
    let _ = GAME_DLL_SPAWN.set(f);
}

/// Registers the real GameDLL `Touch` (call once after DLL load).
pub fn set_game_dll_touch(f: GamedllTouchFn) {
    let _ = GAME_DLL_TOUCH.set(f);
}

/// Registers the real GameDLL `KeyValue` (call once after DLL load).
pub fn set_game_dll_key_value(f: GamedllKeyValueFn) {
    let _ = GAME_DLL_KEY_VALUE.set(f);
}

impl EngineEntities for EngineBackend {
    fn entity_is_valid(&self, index: i32) -> bool {
        unsafe {
            let funcs = (self.engfuncs)();
            let Some(pedict) = (funcs.pfnPEntityOfEntIndex).and_then(|f| f(index).as_mut()) else {
                return false;
            };
            if pedict.free != 0 {
                return false;
            }
            if goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
                // GoldSrc engine: pev->flags & FL_CLIENT (1 << 3 = 8).
                // Edict is a connected client only if FL_CLIENT is set.
                if pedict.v.flags & FL_CLIENT == 0 {
                    return false;
                }
                if pedict.v.netname != 0 {
                    return true;
                }
                if let Some(get_infokey) = funcs.pfnGetInfoKeyBuffer
                    && let Some(infokey_val) = funcs.pfnInfoKeyValue
                {
                    let buffer = get_infokey(pedict);
                    let key = std::ffi::CString::new("name").unwrap_or_default();
                    let val_ptr = infokey_val(buffer, key.as_ptr());
                    if let Some(name_str) = ffi::cstr_to_string_bounded(val_ptr, 64) {
                        return !name_str.is_empty();
                    }
                }
                return true;
            }
            true
        }
    }

    fn entity_classname(&self, index: i32) -> Option<String> {
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = (funcs.pfnPEntityOfEntIndex)?(index);
            if pedict.is_null() {
                return None;
            }
            let classname_offset = (*pedict).v.classname;
            if classname_offset != 0
                && let Some(sz_from_idx) = funcs.pfnSzFromIndex
            {
                let str_ptr = sz_from_idx(classname_offset as i32);
                if let Some(s) = ffi::cstr_to_string_bounded(str_ptr, 64) {
                    return Some(s);
                }
            }
            None
        }
    }

    fn entity_health(&self, index: i32) -> f32 {
        self.get_player(index)
            .map(|e| e.get::<Health>().current())
            .unwrap_or(0.0)
    }

    fn entity_set_health(&self, index: i32, health: f32) {
        if let Some(mut e) = self.get_player(index) {
            e.set(Health::current_only(health));
            // Synchronize HUD health display for human and bot players
            if goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
                let health_msg_id = self.reg_user_msg("Health", 1);
                if health_msg_id > 0 && health_msg_id != 255 {
                    self.message_begin(MessageDest::One as i32, health_msg_id, None, Some(index));
                    self.write_byte(health.clamp(0.0, 255.0) as i32);
                    self.message_end();
                }
            }
        }
    }

    fn entity_origin(&self, index: i32) -> [f32; 3] {
        self.get_player(index)
            .map(|e| e.get::<Origin>().0.into())
            .unwrap_or([0.0; 3])
    }

    fn entity_velocity(&self, index: i32) -> [f32; 3] {
        self.get_player(index)
            .map(|e| e.get::<Velocity>().0.into())
            .unwrap_or([0.0; 3])
    }

    fn entity_set_velocity(&self, index: i32, vel: [f32; 3]) {
        if let Some(mut e) = self.get_player(index) {
            e.set(Velocity(vel.into()));
        }
    }

    fn entity_angles(&self, index: i32) -> [f32; 3] {
        self.get_player(index)
            .map(|e| e.get::<Angles>().0.into())
            .unwrap_or([0.0; 3])
    }

    fn player_name(&self, index: i32) -> Option<String> {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) || !self.entity_is_valid(index) {
            return None;
        }
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = (funcs.pfnPEntityOfEntIndex)?(index);
            if pedict.is_null() {
                return None;
            }
            if let Some(get_infokey) = funcs.pfnGetInfoKeyBuffer
                && let Some(infokey_val) = funcs.pfnInfoKeyValue
            {
                let buffer = get_infokey(pedict);
                let key = std::ffi::CString::new("name").unwrap_or_default();
                let val_ptr = infokey_val(buffer, key.as_ptr());
                if let Some(name) = goldsrc_sys::ffi::cstr_to_string_bounded(val_ptr, 64) {
                    return Some(name);
                }
            }
            let netname_offset = (*pedict).v.netname;
            if netname_offset != 0
                && let Some(sz_from_idx) = funcs.pfnSzFromIndex
            {
                let str_ptr = sz_from_idx(netname_offset as i32);
                if let Some(name) = goldsrc_sys::ffi::cstr_to_string_bounded(str_ptr, 64) {
                    return Some(name);
                }
            }
            None
        }
    }

    fn player_lang(&self, index: i32) -> Option<String> {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return None;
        }
        if let Some(session_lang) = crate::host::HostRuntime::with_sessions(|s| {
            s.get(index)
                .and_then(|sess| sess.lang().map(str::to_string))
        })
        .flatten()
        {
            return Some(session_lang);
        }
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = (funcs.pfnPEntityOfEntIndex)?(index);
            if pedict.is_null() {
                return None;
            }
            if let Some(get_infokey) = funcs.pfnGetInfoKeyBuffer
                && let Some(infokey_val) = funcs.pfnInfoKeyValue
            {
                let buffer = get_infokey(pedict);
                for key_name in ["_lang", "lang", "_cl_lang", "cl_lang", "language"] {
                    let key = std::ffi::CString::new(key_name).unwrap_or_default();
                    let val_ptr = infokey_val(buffer, key.as_ptr());
                    if let Some(lang) = goldsrc_sys::ffi::cstr_to_string_bounded(val_ptr, 16) {
                        return Some(lang.to_lowercase());
                    }
                }
            }
            self.cvar_get_string("server_language")
        }
    }

    fn player_team(&self, index: i32) -> i32 {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) || !self.entity_is_valid(index) {
            return 0;
        }
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = match funcs.pfnPEntityOfEntIndex {
                Some(f) => f(index),
                None => return 0,
            };
            if pedict.is_null() {
                return 0;
            }

            // Universal GoldSrc entity team index
            (*pedict).v.team
        }
    }

    fn player_auth_id(&self, index: i32) -> Option<String> {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) || !self.entity_is_valid(index) {
            return None;
        }
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = (funcs.pfnPEntityOfEntIndex)?(index);
            if pedict.is_null() {
                return None;
            }
            if let Some(get_auth) = funcs.pfnGetPlayerAuthId {
                let auth_ptr = get_auth(pedict);
                if let Some(auth_str) = goldsrc_sys::ffi::cstr_to_string_bounded(auth_ptr, 64) {
                    let trimmed = auth_str.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
            None
        }
    }

    fn player_user_id(&self, index: i32) -> u32 {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) || !self.entity_is_valid(index) {
            return 0;
        }
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = match funcs.pfnPEntityOfEntIndex {
                Some(f) => f(index),
                None => return 0,
            };
            if pedict.is_null() {
                return 0;
            }
            if let Some(get_userid) = funcs.pfnGetPlayerUserId {
                let uid = get_userid(pedict);
                if uid > 0 {
                    return uid as u32;
                }
            }
            0
        }
    }

    fn player_ip(&self, index: i32) -> Option<String> {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) || !self.entity_is_valid(index) {
            return None;
        }
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = (funcs.pfnPEntityOfEntIndex)?(index);
            if pedict.is_null() {
                return None;
            }
            if let Some(get_infokey) = funcs.pfnGetInfoKeyBuffer
                && let Some(infokey_val) = funcs.pfnInfoKeyValue
            {
                let buffer = get_infokey(pedict);
                let key = std::ffi::CString::new("ip").unwrap_or_default();
                let val_ptr = infokey_val(buffer, key.as_ptr());
                if let Some(raw_ip) = goldsrc_sys::ffi::cstr_to_string_bounded(val_ptr, 64) {
                    let cleaned = raw_ip.trim();
                    if !cleaned.is_empty() {
                        // Strip trailing port if present (e.g. "192.168.1.10:27005" -> "192.168.1.10")
                        let ip_only = cleaned.split(':').next().unwrap_or(cleaned);
                        return Some(ip_only.to_string());
                    }
                }
            }
            None
        }
    }

    fn player_identity(&self, index: i32) -> goldsrc_api::client::PlayerIdentity {
        let mut ping = 0;
        let mut loss = 0;
        let mut is_bot = false;
        let mut is_hltv = false;

        unsafe {
            let funcs = (self.engfuncs)();
            if let Some(pedict) = (funcs.pfnPEntityOfEntIndex).and_then(|f| f(index).as_mut()) {
                if let Some(get_stats) = funcs.pfnGetPlayerStats {
                    let mut p = 0;
                    let mut l = 0;
                    get_stats(pedict, &mut p, &mut l);
                    ping = p;
                    loss = l;
                }
                let flags = pedict.v.flags;
                is_bot = (flags & goldsrc_api::consts::FL_FAKECLIENT) != 0;
                is_hltv = (flags & goldsrc_api::consts::FL_PROXY) != 0;
            }
        }

        let steam_id_raw = self.player_auth_id(index);
        let (raw_auth_id, auth_state) = match steam_id_raw {
            Some(raw) => {
                let trimmed = raw.trim();
                let state =
                    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("STEAM_ID_PENDING") {
                        goldsrc_api::client::AuthState::Pending
                    } else if is_bot || trimmed.eq_ignore_ascii_case("BOT") {
                        let bot_name = self
                            .player_name(index)
                            .unwrap_or_else(|| format!("Bot #{}", index));
                        goldsrc_api::client::AuthState::Authenticated(
                            goldsrc_api::client::AuthSubject::bot(bot_name),
                        )
                    } else if is_hltv || trimmed.eq_ignore_ascii_case("HLTV") {
                        goldsrc_api::client::AuthState::Authenticated(
                            goldsrc_api::client::AuthSubject::hltv(),
                        )
                    } else if trimmed.eq_ignore_ascii_case("STEAM_ID_LAN")
                        || trimmed.eq_ignore_ascii_case("VALVE_ID_LAN")
                    {
                        let ip: std::net::IpAddr = self
                            .player_ip(index)
                            .and_then(|s| s.parse().ok())
                            .unwrap_or_else(|| "127.0.0.1".parse().unwrap());
                        AuthState::Authenticated(AuthSubject::lan(ip))
                    } else if let Some(steam_id) = SteamId::parse(trimmed) {
                        AuthState::Authenticated(AuthSubject::steam(steam_id))
                    } else {
                        AuthState::Authenticated(AuthSubject::external("custom", trimmed))
                    };
                (raw, state)
            }
            None if is_bot => {
                let bot_name = self
                    .player_name(index)
                    .unwrap_or_else(|| format!("Bot #{}", index));
                (
                    "BOT".to_string(),
                    goldsrc_api::client::AuthState::Authenticated(
                        goldsrc_api::client::AuthSubject::bot(bot_name),
                    ),
                )
            }
            None if is_hltv => (
                "HLTV".to_string(),
                AuthState::Authenticated(AuthSubject::hltv()),
            ),
            None => ("STEAM_ID_PENDING".to_string(), AuthState::Pending),
        };

        PlayerIdentity {
            slot: index,
            user_id: self.player_user_id(index),
            raw_auth_id,
            auth_state,
            ip: self.player_ip(index),
            ping,
            packet_loss: loss,
            is_bot,
            is_hltv,
        }
    }

    fn player_armorvalue(&self, index: i32) -> f32 {
        self.get_player(index)
            .map(|p| p.get::<Armor>().value())
            .unwrap_or(0.0)
    }

    fn player_set_armorvalue(&self, index: i32, armor: f32) {
        if let Some(mut p) = self.get_player(index) {
            p.set(Armor::new(armor));
            // Synchronize HUD armor display for human and bot players
            if goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
                let battery_msg_id = self.reg_user_msg("Battery", 2);
                if battery_msg_id > 0 && battery_msg_id != 255 {
                    self.message_begin(MessageDest::One as i32, battery_msg_id, None, Some(index));
                    self.write_short(armor.clamp(0.0, 255.0) as i32);
                    self.message_end();
                }
            }
        }
    }

    fn remove_entity(&self, index: i32) {
        unsafe {
            let funcs = (self.engfuncs)();
            let pent = funcs.pfnPEntityOfEntIndex.and_then(|f| {
                let p = f(index);
                if p.is_null() { None } else { Some(p) }
            });
            if let Some(p) = pent {
                call_engfunc!(funcs.pfnRemoveEntity, p);
            }
        }
    }

    fn drop_to_floor(&self, index: i32) -> i32 {
        unsafe {
            let funcs = (self.engfuncs)();
            let pent = funcs.pfnPEntityOfEntIndex.and_then(|f| {
                let p = f(index);
                if p.is_null() { None } else { Some(p) }
            });
            if let Some(p) = pent {
                call_engfunc_ret!(funcs.pfnDropToFloor, p)
            } else {
                0
            }
        }
    }

    fn dispatch_touch(&self, touched: i32, other: i32) {
        unsafe {
            let funcs = (self.engfuncs)();
            let resolve = |idx: i32| {
                funcs.pfnPEntityOfEntIndex.and_then(|f| {
                    let p = f(idx);
                    if p.is_null() { None } else { Some(p) }
                })
            };
            match (resolve(touched), resolve(other), GAME_DLL_TOUCH.get()) {
                (Some(a), Some(b), Some(f)) => f(a, b),
                _ => {
                    log::debug!(target: CORE, "dispatch_touch({touched},{other}): no GameDLL bridge");
                }
            }
        }
    }

    fn set_client_listening(&self, receiver: i32, sender: i32, listen: bool) -> bool {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&receiver)
            || !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&sender)
        {
            return false;
        }
        unsafe {
            let funcs = (self.engfuncs)();
            if let Some(pfn_voice_set_client_listening) = funcs.pfnVoice_SetClientListening {
                let res = pfn_voice_set_client_listening(
                    receiver as std::os::raw::c_int,
                    sender as std::os::raw::c_int,
                    if listen { 1 } else { 0 },
                );
                return res != 0;
            }
            false
        }
    }

    fn set_player_maxspeed(&self, index: i32, speed: f32) {
        if !goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            return;
        }
        unsafe {
            let funcs = (self.engfuncs)();
            if let Some(pfn_p_entity_of_ent_index) = funcs.pfnPEntityOfEntIndex {
                let pedict = pfn_p_entity_of_ent_index(index);
                if !pedict.is_null() {
                    (*pedict).v.maxspeed = speed;
                }
            }
        }
    }
}

impl EntitySpawner for EngineBackend {
    fn create_named_entity(&self, classname: &str) -> Option<i32> {
        unsafe {
            let funcs = (self.engfuncs)();
            let cstr = std::ffi::CString::new(classname).ok()?;
            let str_id = funcs.pfnAllocString.map(|f| f(cstr.as_ptr())).unwrap_or(0);
            if str_id == 0 {
                return None;
            }
            let pent = funcs.pfnCreateNamedEntity.map(|f| f(str_id))?;
            if pent.is_null() {
                return None;
            }

            let idx = crate::api_registry::edict_index(pent);
            if idx > 0 { Some(idx) } else { None }
        }
    }

    fn entity_set_origin(&self, index: i32, pos: [f32; 3]) {
        if let Some(mut e) = self.get_player(index) {
            e.set(goldsrc_api::Origin(pos.into()));
        }
    }

    fn entity_set_angles(&self, index: i32, angles: [f32; 3]) {
        if let Some(mut e) = self.get_player(index) {
            e.set(goldsrc_api::Angles(angles.into()));
        }
    }

    fn dispatch_spawn(&self, index: i32) -> i32 {
        unsafe {
            let funcs = (self.engfuncs)();
            let pent = funcs.pfnPEntityOfEntIndex.and_then(|f| {
                let p = f(index);
                if p.is_null() { None } else { Some(p) }
            });
            match (pent, GAME_DLL_SPAWN.get()) {
                (Some(p), Some(f)) => f(p),
                _ => {
                    log::debug!(target: CORE, "dispatch_spawn({index}): no GameDLL bridge");
                    0
                }
            }
        }
    }

    fn entity_key_value(&self, index: i32, key: &str, value: &str) -> bool {
        unsafe {
            let funcs = (self.engfuncs)();
            let pent = match funcs.pfnPEntityOfEntIndex {
                Some(f) => f(index),
                None => return false,
            };
            if pent.is_null() {
                return false;
            }

            let classname =
                <Self as EngineEntities>::entity_classname(self, index).unwrap_or_default();
            let c_class = std::ffi::CString::new(classname).unwrap_or_default();
            let c_key = std::ffi::CString::new(key).unwrap_or_default();
            let c_val = std::ffi::CString::new(value).unwrap_or_default();

            let mut kvd = KeyValueData {
                szClassName: c_class.as_ptr(),
                szKeyName: c_key.as_ptr(),
                szValue: c_val.as_ptr(),
                fHandled: 0,
            };

            if let Some(kv_fn) = GAME_DLL_KEY_VALUE.get() {
                kv_fn(pent, &mut kvd);
                kvd.fHandled != 0
            } else {
                false
            }
        }
    }
}
