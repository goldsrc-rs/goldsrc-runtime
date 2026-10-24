//! Engine function table bridge implementing `goldsrc_spi::engine::EnginePort`.

use crate::backend::print_queue::PrintQueue;
use crate::call_engfunc;
use goldsrc_api::client::Player;
use goldsrc_api::consts::FL_CLIENT;
use goldsrc_spi::engine::{EngineConsole, EngineEntities};
use goldsrc_sys::enginefuncs_t;

pub mod console;
pub mod cvars;
pub mod entities;
pub mod extensions;
pub mod messages;
pub mod physics;
pub mod precache;
pub mod sound;

pub use cvars::{MapNameResolverFn, set_map_name_resolver};
pub use entities::{
    GamedllKeyValueFn, GamedllSpawnFn, GamedllTouchFn, set_game_dll_key_value, set_game_dll_spawn,
    set_game_dll_touch,
};
pub use messages::{UserMsgResolverFn, register_user_msg_id, set_user_msg_resolver};

/// Standard `Engine` implementation parameterized by the engfunc source.
#[derive(Clone, Copy)]
pub struct EngineBackend {
    pub(crate) engfuncs: fn() -> &'static enginefuncs_t,
    pub(crate) print_queue: &'static PrintQueue,
}

impl EngineBackend {
    /// Create a backend from an engfunc-table accessor and a print queue.
    pub const fn new(
        engfuncs: fn() -> &'static enginefuncs_t,
        print_queue: &'static PrintQueue,
    ) -> Self {
        Self {
            engfuncs,
            print_queue,
        }
    }

    /// Creates a player handle from an index if valid.
    ///
    /// For player slots (1..=32) the edict must have `FL_CLIENT` (1 << 3) set;
    /// engine slots without a connected client will not have this flag even when
    /// `edict.free == 0`, so we reject them here before handing off to
    /// `EDict::is_valid()` which only checks the serial number.
    pub fn get_player(&self, index: i32) -> Option<Player> {
        unsafe {
            let funcs = (self.engfuncs)();
            let edict = (funcs.pfnPEntityOfEntIndex).and_then(|f| f(index).as_mut())?;
            if edict.free != 0 {
                return None;
            }
            if goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index)
                && edict.v.flags & FL_CLIENT == 0
            {
                return None;
            }
            Some(Player::from_raw(index, edict))
        }
    }

    /// Counts connected active human and bot players from engine edicts.
    pub fn count_active_players(&self) -> usize {
        let auth_count = goldsrc_api::auth::Auth::total_players();
        if auth_count > 0 {
            return auth_count;
        }
        let mut count = 0;
        for i in 1..=(goldsrc_api::consts::MAX_PLAYERS as i32) {
            if <Self as EngineEntities>::player_name(self, i).is_some() {
                count += 1;
            }
        }
        count
    }

    /// Spawns an entity by classname.
    pub fn spawn_entity(&self, classname: &str) -> Option<goldsrc_api::Entity> {
        unsafe {
            let funcs = (self.engfuncs)();
            let edict = (funcs.pfnCreateEntity)?();
            if edict.is_null() {
                return None;
            }
            let cname = std::ffi::CString::new(classname).unwrap_or_default();
            if let Some(alloc_string) = funcs.pfnAllocString {
                (*edict).v.classname = alloc_string(cname.as_ptr()) as u32;
            }
            let index = crate::api_registry::edict_index(edict);
            Some(goldsrc_api::Entity::from_raw(index, edict))
        }
    }

    /// Prints a message to the server console.
    pub fn server_print(&self, message: &str) {
        <Self as EngineConsole>::server_print(self, message);
    }

    /// Executes a server command string.
    pub fn server_command(&self, command: &str) {
        <Self as EngineConsole>::server_command(self, command);
    }

    /// Drains the deferred server-print queue to the engine console with
    /// fmtlib-safe escaping (ReHLDS routes `ServerPrint` through fmtlib).
    pub fn drain_prints(&self) {
        for message in self.print_queue.drain() {
            if let Ok(cstr) = std::ffi::CString::new(message) {
                unsafe {
                    call_engfunc!((self.engfuncs)().pfnServerPrint, cstr.as_ptr());
                }
            }
        }
    }
}
