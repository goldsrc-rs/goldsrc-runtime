//! Metamod backend implementation for GoldSrc.rs.

// ============================================================================
// MSVC Linker Directives
//
// On i686-pc-windows-msvc, __stdcall functions are decorated by the linker as
// `_FunctionName@N` (where N is the argument byte count). The GoldSrc engine
// looks for undecorated names (e.g. `GiveFnptrsToDll`), so we must instruct
// the linker to export the decorated symbol under its clean name.
//
// This replaces the legacy `exports.c` / `#pragma comment(linker, ...)` C file.
// The `.drectve` section is the standard COFF mechanism for embedding linker
// directives directly inside an object file — exactly what the C pragma did.
// ============================================================================
mod commands;
mod entrypoints;
mod hooks;
mod meta_types;

use goldsrc_core::log;

use meta_types::*;

static G_ENGFUNCS: std::sync::OnceLock<
    goldsrc_sys::ffi::SyncWrapper<&'static goldsrc_sys::enginefuncs_t>,
> = std::sync::OnceLock::new();
static G_GLOBALS: std::sync::OnceLock<
    goldsrc_sys::ffi::SyncWrapper<&'static goldsrc_sys::globalvars_t>,
> = std::sync::OnceLock::new();

/// Deferred server-print queue shared with the standalone backend.
pub static PRINT_QUEUE: goldsrc_core::backend::PrintQueue =
    goldsrc_core::backend::PrintQueue::new();

/// Initialize WASM plugin subsystem and the unified logger.
pub fn init_wasm_host() {
    goldsrc_core::backend::set_map_name_resolver(|| {
        if let Some(wrapped) = G_GLOBALS.get() {
            let globals = **wrapped;
            let mapname_str_offset = globals.mapname;
            if mapname_str_offset != 0 {
                // 1. Direct memory resolution via pStringBase (standard HLSDK STRING() macro)
                if !globals.pStringBase.is_null()
                    && (mapname_str_offset as usize).wrapping_add(64)
                        <= goldsrc_sys::ffi::STRING_POOL_MAX
                {
                    let ptr = unsafe {
                        (globals.pStringBase as *const u8).add(mapname_str_offset as usize)
                            as *const std::os::raw::c_char
                    };
                    if let Some(name) = unsafe { goldsrc_sys::ffi::cstr_to_string_bounded(ptr, 64) }
                    {
                        return Some(name);
                    }
                }
                // 2. Engine string table resolver via pfnSzFromIndex
                if let Some(sz_fn) = engfuncs().pfnSzFromIndex {
                    let ptr = unsafe { sz_fn(mapname_str_offset as i32) };
                    if let Some(name) = unsafe { goldsrc_sys::ffi::cstr_to_string_bounded(ptr, 64) }
                    {
                        return Some(name);
                    }
                }
            }
        }
        None
    });

    goldsrc_core::backend::set_user_msg_resolver(|name| {
        goldsrc_extension_metamod::MetamodApi::get_user_msg_id(name).unwrap_or(0)
    });
    let engine: std::sync::Arc<dyn goldsrc_spi::engine::EnginePort> = std::sync::Arc::new(
        goldsrc_core::backend::EngineBackend::new(engfuncs, &PRINT_QUEUE),
    );
    #[cfg(feature = "legacy-reapi")]
    goldsrc_extension_reapi::init();
    goldsrc_extension_metamod::init(true);
    if let Err(e) = goldsrc_core::host::HostRuntime::init(
        goldsrc_api::consts::BackendType::Metamod,
        |msg| {
            backend().server_print(msg);
        },
        engine,
    ) {
        log::error!(target: goldsrc_api::consts::log_targets::CORE, "{e}");
    }
}

/// # Safety
/// Called once from `GiveFnptrsToDll`.
pub unsafe fn init_backend(
    engfuncs: *mut goldsrc_sys::enginefuncs_t,
    globals: *mut goldsrc_sys::globalvars_t,
) {
    if !engfuncs.is_null() {
        // SAFETY: engfuncs is checked for null
        let _ = G_ENGFUNCS.set(goldsrc_sys::ffi::SyncWrapper::new(unsafe { &*engfuncs }));
    }
    if !globals.is_null() {
        // SAFETY: globals is checked for null
        let _ = G_GLOBALS.set(goldsrc_sys::ffi::SyncWrapper::new(unsafe { &*globals }));
    }
}

static DUMMY_ENGFUNCS: goldsrc_sys::enginefuncs_t = unsafe { std::mem::zeroed() };
static DUMMY_GLOBALS: goldsrc_sys::ffi::SyncWrapper<goldsrc_sys::globalvars_t> =
    goldsrc_sys::ffi::SyncWrapper::new(unsafe { std::mem::zeroed() });
static mut DUMMY_META_GLOBALS: meta_globals_t = unsafe { std::mem::zeroed() };

pub fn engfuncs() -> &'static goldsrc_sys::enginefuncs_t {
    G_ENGFUNCS.get().map(|s| **s).unwrap_or(&DUMMY_ENGFUNCS)
}

pub fn globals() -> &'static goldsrc_sys::globalvars_t {
    G_GLOBALS.get().map(|s| **s).unwrap_or(&*DUMMY_GLOBALS)
}

pub fn meta_globals() -> &'static mut meta_globals_t {
    goldsrc_extension_metamod::MetamodApi::raw_meta_globals()
        .unwrap_or_else(|| unsafe { &mut *std::ptr::addr_of_mut!(DUMMY_META_GLOBALS) })
}

static G_GAMEDLL_FUNCS: std::sync::atomic::AtomicPtr<gamedll_funcs_t> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

pub fn set_meta_globals(ptr: *mut meta_globals_t) {
    goldsrc_extension_metamod::set_meta_globals(ptr);
}

/// # Safety
/// `ptr` must be valid or null (passed from Metamod).
pub unsafe fn set_meta_util_funcs(ptr: *mut mutil_funcs_t) {
    goldsrc_extension_metamod::set_meta_util_funcs(ptr, &entrypoints::PLUGIN_INFO);
}

/// # Safety
/// `ptr` must be valid or null (passed from Metamod).
pub unsafe fn set_gamedll_funcs(ptr: *mut gamedll_funcs_t) {
    G_GAMEDLL_FUNCS.store(ptr, std::sync::atomic::Ordering::Relaxed);
}

/// Resolves real GameDLL spawn and touch function pointers from Metamod after server activation.
pub fn ensure_game_dll_hooks() {
    let ptr = G_GAMEDLL_FUNCS.load(std::sync::atomic::Ordering::Relaxed);
    if !ptr.is_null() {
        // SAFETY: gamedll_funcs provides direct pointers to real GameDLL tables
        unsafe {
            let dllapi = (*ptr).dllapi_table;
            if !dllapi.is_null() {
                if let Some(spawn_fn) = (*dllapi).pfnSpawn {
                    goldsrc_core::backend::set_game_dll_spawn(spawn_fn);
                }
                if let Some(touch_fn) = (*dllapi).pfnTouch {
                    goldsrc_core::backend::set_game_dll_touch(touch_fn);
                }
                if let Some(kv_fn) = (*dllapi).pfnKeyValue {
                    goldsrc_core::backend::set_game_dll_key_value(kv_fn);
                }
            }
        }
    }
}

use goldsrc_core::backend::EngineBackend;

/// Metamod backend: the shared `EngineBackend` fed by this crate's engfunc
/// accessor and print queue. The backend is a thin adapter.
pub type MetamodBackend = EngineBackend;

/// Metamod adapter marking this backend as an SMA adapter.
#[stitch_rs::adapter]
pub struct MetamodAdapter;

impl stitch_rs::Adapter for MetamodAdapter {
    type TargetPort = ();
}

pub use goldsrc_core::call_engfunc;
pub use goldsrc_core::call_engfunc_ret;

static BACKEND: MetamodBackend = EngineBackend::new(engfuncs, &PRINT_QUEUE);

pub fn backend() -> &'static MetamodBackend {
    &BACKEND
}

pub use entrypoints::{
    GetEngineFunctions, GetEngineFunctions_Post, GetEntityAPI, GetEntityAPI_Post, GetEntityAPI2,
    GetEntityAPI2_Post, GetNewDLLFunctions, GetNewDLLFunctions_Post, GiveFnptrsToDll, Meta_Attach,
    Meta_Detach, Meta_Query,
};
