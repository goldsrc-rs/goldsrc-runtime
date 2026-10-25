//! Shared backend plumbing: engine access, deferred print queue and engfunc-call macros.

pub mod console_hook;
pub mod engine_bridge;
pub mod print_queue;

pub use engine_bridge::{
    EngineBackend, GamedllKeyValueFn, GamedllSpawnFn, GamedllTouchFn, MapNameResolverFn,
    UserMsgResolverFn, register_user_msg_id, set_game_dll_key_value, set_game_dll_spawn,
    set_game_dll_touch, set_map_name_resolver, set_user_msg_resolver,
};
pub use print_queue::{PrintQueue, cstr_to_string, escape_server_print, sanitize_client_print};

/// Invokes an optional engfunc pointer with no arguments.
#[macro_export]
macro_rules! call_engfunc {
    ($func:expr) => {
        if let Some(f) = $func {
            f();
        }
    };
    ($func:expr, $($arg:expr),*) => {
        if let Some(f) = $func {
            f($($arg),*);
        }
    };
}

/// Invokes an optional engfunc pointer and returns its result, or `Default::default()` if unset.
#[macro_export]
macro_rules! call_engfunc_ret {
    ($func:expr) => {
        if let Some(f) = $func {
            f()
        } else {
            Default::default()
        }
    };
    ($func:expr, $($arg:expr),*) => {
        if let Some(f) = $func {
            f($($arg),*)
        } else {
            Default::default()
        }
    };
}
