//! Metamod `#[unsafe(no_mangle)]` entry points (FFI boundary).

use abi_barrier::prelude::*;
use std::ffi::c_void;

use crate::meta_types::*;
use crate::{backend, init_backend, init_wasm_host};

/// # Safety
/// Called by the engine during DLL loading. `engfuncs` and `globals` are
/// valid pointers for the lifetime of the server process.
/// Any Rust panic is caught — an unhandled panic here would crash HLDS.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::System, on_panic = PanicStrategy::ReturnDefault)]
pub unsafe fn GiveFnptrsToDll(
    engfuncs: *mut goldsrc_sys::enginefuncs_t,
    globals: *mut goldsrc_sys::globalvars_t,
) {
    // SAFETY: engfuncs and globals are engine-provided; valid for the server lifetime.
    goldsrc_sys::guard::install_crash_guard();
    goldsrc_core::backend::console_hook::init();
    if !engfuncs.is_null()
        && let Some(cmd_fn) = unsafe { (*engfuncs).pfnServerCommand }
    {
        goldsrc_core::backend::console_hook::register_shutdown_handler(cmd_fn);
    }
    unsafe { init_backend(engfuncs, globals) };
    backend().server_print("[GoldSrc.rs] Engine functions received.\n");
}

/// # Safety
/// Called by Metamod during plugin loading. Pointers must be valid.
/// Any Rust panic is caught — the C caller receives 0 instead of UB.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn Meta_Query(
    _ifvers: *const std::os::raw::c_char,
    plugin_info: *mut *const plugin_info_t,
    meta_util_functions: *mut mutil_funcs_t,
) -> std::os::raw::c_int {
    // SAFETY: plugin_info and meta_util_functions are Metamod-provided; valid at call time.
    goldsrc_core::backend::console_hook::init();
    unsafe {
        if plugin_info.is_null() {
            return 0;
        }
        *plugin_info = &PLUGIN_INFO;
        if !meta_util_functions.is_null() {
            crate::set_meta_util_funcs(meta_util_functions);
        }
    }
    backend().server_print("[GoldSrc.rs] Meta_Query called.\n");
    1
}

/// # Safety
/// Called by Metamod after Meta_Query. Pointers must be valid.
/// Any Rust panic is caught — the C caller receives 0 instead of UB.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn Meta_Attach(
    _now: PLUG_LOADTIME,
    meta_functions: *mut meta_function_t,
    meta_globals: *mut meta_globals_t,
    gamedll_funcs: *mut gamedll_funcs_t,
) -> std::os::raw::c_int {
    // SAFETY: meta_functions and meta_globals are Metamod-provided; valid at call time.
    goldsrc_core::backend::console_hook::init();
    if let Some(cmd_fn) = crate::engfuncs().pfnServerCommand {
        goldsrc_core::backend::console_hook::register_shutdown_handler(cmd_fn);
    }
    unsafe {
        if meta_globals.is_null() || meta_functions.is_null() {
            return 0;
        }
        crate::set_meta_globals(meta_globals);
        if !gamedll_funcs.is_null() {
            crate::set_gamedll_funcs(gamedll_funcs);
            crate::ensure_game_dll_hooks();
        }

        // Fill the META_FUNCTIONS table with our hook functions.
        (*meta_functions).pfnGetEntityAPI2 = Some(crate::GetEntityAPI2);
        (*meta_functions).pfnGetEntityAPI2_Post = Some(crate::GetEntityAPI2_Post);
        (*meta_functions).pfnGetEngineFunctions_Post = Some(GetEngineFunctions_Post);

        // Register the unified hook strategy before any table query.
        goldsrc_core::api_registry::register(goldsrc_core::api_registry::Registry {
            hooks: &crate::hooks::HOOKS,
            engfuncs: crate::engfuncs,
        });
    }
    init_wasm_host();
    crate::commands::register_cli_commands();
    backend().server_print("[GoldSrc.rs] Meta_Attach called.\n");
    backend().server_print("[GoldSrc.rs] WASM Host Engine initialized.\n");
    backend()
        .server_print("[GoldSrc.rs] Host Management CLI registered (`meta-rs` / `goldsrc`).\n");
    1
}

/// # Safety
/// Called by Metamod during plugin unloading.
/// Any Rust panic is caught — the C caller receives 0 instead of UB.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn Meta_Detach(_now: PLUG_LOADTIME, _reason: PL_UNLOAD_REASON) -> std::os::raw::c_int {
    backend().server_print("[GoldSrc.rs] Meta_Detach called.\n");
    1
}

#[allow(non_upper_case_globals)]
pub static PLUGIN_INFO: plugin_info_t = plugin_info_t {
    ifvers: META_INTERFACE_VERSION.as_ptr(),
    name: c"GoldSrc.rs Metamod Backend".as_ptr(),
    version: concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const i8,
    date: concat!(env!("GIT_HASH"), "\0").as_ptr() as *const i8,
    author: c"GoldSrc.rs Contributors".as_ptr(),
    url: c"https://github.com/ulquiorracode/GoldSrc.rs".as_ptr(),
    logtag: c"GOLDSRC.RS".as_ptr(),
    loadable: PLUG_LOADTIME::PT_ANYTIME,
    unloadable: PLUG_LOADTIME::PT_ANYTIME,
};

// ============================================================================
// Entity API entry points
// ============================================================================

/// Function tables that we provide to Metamod.
/// Metamod calls these to get our hook functions.
/// # Safety
/// Called by Metamod to get entity API hooks. Pointers must be valid.
/// Note: This is only called when the plugin is loaded as a game DLL plugin.
/// Any Rust panic is caught and contained — the C caller receives 0 instead of UB.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn GetEntityAPI2(
    dll_table: *mut goldsrc_sys::DLL_FUNCTIONS,
    interface_version: *mut i32,
) -> i32 {
    // SAFETY: dll_table and interface_version are engine-provided; valid at call time.
    if unsafe {
        goldsrc_core::api_registry::ApiRegistry::install(
            dll_table,
            interface_version,
            goldsrc_core::api_registry::HookPhase::Pre,
            true,
        )
    } {
        1
    } else {
        0
    }
}

/// # Safety
/// Called by Metamod to get post-entity API hooks.
/// Any Rust panic is caught — the C caller receives 0 instead of UB.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn GetEntityAPI2_Post(
    dll_table: *mut goldsrc_sys::DLL_FUNCTIONS,
    interface_version: *mut i32,
) -> i32 {
    // SAFETY: dll_table and interface_version are engine-provided; valid at call time.
    if unsafe {
        goldsrc_core::api_registry::ApiRegistry::install(
            dll_table,
            interface_version,
            goldsrc_core::api_registry::HookPhase::Post,
            true,
        )
    } {
        1
    } else {
        0
    }
}

/// # Safety
/// Called by Metamod to get entity API hooks (old interface).
/// Any Rust panic is caught — the C caller receives 0 instead of UB.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn GetEntityAPI(
    dll_table: *mut goldsrc_sys::DLL_FUNCTIONS,
    interface_version: i32,
) -> i32 {
    if dll_table.is_null() || interface_version != goldsrc_api::consts::ENGINE_INTERFACE_VERSION {
        return 0;
    }
    backend().server_print("[GoldSrc.rs] GetEntityAPI called.\n");
    1
}

/// # Safety
/// Called by Metamod to get post-entity API hooks (old interface).
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn GetEntityAPI_Post(
    _dll_table: *mut goldsrc_sys::DLL_FUNCTIONS,
    _interface_version: i32,
) -> i32 {
    0
}

/// # Safety
/// Called by Metamod to get new DLL functions.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn GetNewDLLFunctions(_new_table: *mut c_void, _interface_version: *mut i32) -> i32 {
    0
}

/// # Safety
/// Called by Metamod to get post-new DLL functions.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn GetNewDLLFunctions_Post(
    _new_table: *mut c_void,
    _interface_version: *mut i32,
) -> i32 {
    0
}

/// # Safety
/// Called by Metamod to get engine functions. Pointers must be valid.
/// Any Rust panic is caught — the C caller receives 0 instead of UB.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn GetEngineFunctions(
    _engfuncs: *mut goldsrc_sys::enginefuncs_t,
    _interface_version: *mut i32,
) -> i32 {
    0
}

#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
unsafe fn hook_reg_user_msg_post(
    psz_name: *const std::os::raw::c_char,
    _i_size: std::os::raw::c_int,
) -> std::os::raw::c_int {
    let msg_id = goldsrc_extension_metamod::MetamodApi::orig_ret_val::<i32>().unwrap_or(0);
    if !psz_name.is_null()
        && msg_id > 0
        && msg_id != 255
        && let Ok(c_str) = unsafe { std::ffi::CStr::from_ptr(psz_name) }.to_str()
    {
        goldsrc_core::backend::register_user_msg_id(c_str, msg_id);
    }
    msg_id
}

/// # Safety
/// Called by Metamod to get post-engine functions.
#[unsafe(no_mangle)]
#[inline(never)]
#[barrier(convention = CallingConvention::C, on_panic = PanicStrategy::ReturnError(0))]
pub unsafe fn GetEngineFunctions_Post(
    engfuncs: *mut goldsrc_sys::enginefuncs_t,
    _interface_version: *mut i32,
) -> i32 {
    if engfuncs.is_null() {
        return 0;
    }
    unsafe {
        (*engfuncs).pfnRegUserMsg = Some(hook_reg_user_msg_post);
    }
    1
}
