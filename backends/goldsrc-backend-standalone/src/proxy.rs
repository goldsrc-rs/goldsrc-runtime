//! GoldSrc.rs Standalone Backend — proxy GameDLL loader.
//!
//! Loads the real game DLL (`mp.dll` / `cs.so`) and forwards all standard
//! GameDLL exports to it, while inserting our hooks around the key callbacks.

use goldsrc_api::consts::log_targets;
use goldsrc_core::log;
use goldsrc_sys::{DLL_FUNCTIONS, NEW_DLL_FUNCTIONS, enginefuncs_t, globalvars_t};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Name of the real game DLL to proxy.
#[cfg(target_os = "windows")]
const GAME_DLL_NAMES: &[&str] = &[
    "mp.dll",
    "server.dll",
    "hl.dll",
    "cs.so",
    "svencoop.dll",
    "dod.dll",
    "tfc.dll",
];

#[cfg(target_os = "linux")]
const GAME_DLL_NAMES: &[&str] = &["cs.so", "server.so", "hl.so", "dod.so", "tfc.so"];

/// Holds the loaded real game DLL and its resolved function tables.
pub struct GameDllProxy {
    /// Libloading handle — keeps the DLL alive.
    _lib: libloading::Library,
    /// DLL function table populated by the real game DLL.
    pub dll_funcs: DLL_FUNCTIONS,
    /// Optional NEW_DLL_FUNCTIONS table (ReGameDLL, Sven Co-op).
    pub new_dll_funcs: NEW_DLL_FUNCTIONS,
    pub has_new_dll_funcs: bool,
    /// Whether the real DLL was successfully loaded.
    #[allow(dead_code)]
    pub loaded: bool,
}

static PROXY: OnceLock<std::sync::Mutex<GameDllProxy>> = OnceLock::new();

/// Ensure the real game DLL is loaded and its function tables are populated.
pub fn ensure_loaded() -> bool {
    if PROXY.get().is_some() {
        return true;
    }
    let dll_path = resolve_game_dll_path();
    let norm_path = goldsrc_core::paths::PathResolver::normalize(&dll_path);
    log::info!(
        target: log_targets::PROXY,
        "Attempting to load real GameDLL from path: \"{}\"",
        norm_path
    );

    let result = unsafe { try_load_game_dll(&dll_path) };

    match result {
        Ok(proxy) => {
            log::info!(
                target: log_targets::PROXY,
                "Successfully loaded real GameDLL from \"{}\"",
                norm_path
            );
            let _ = PROXY.set(std::sync::Mutex::new(proxy));
            true
        }
        Err(e) => {
            log::error!(
                target: log_targets::PROXY,
                "ERROR: Failed to load real GameDLL from \"{}\": {}",
                norm_path,
                e
            );
            eprintln!(
                "[GoldSrc.rs Standalone] WARNING: Failed to load real game DLL \"{}\": {}",
                norm_path, e
            );
            false
        }
    }
}

/// Forward GiveFnptrsToDll to the real game DLL.
///
/// # Safety
/// `engfuncs` and `globals` must be valid engine pointers.
pub unsafe fn forward_give_fnptrs_to_dll(engfuncs: *mut enginefuncs_t, globals: *mut globalvars_t) {
    ensure_loaded();
    if let Some(proxy_lock) = PROXY.get() {
        let guard = proxy_lock.lock().unwrap_or_else(|e| e.into_inner());
        let give_fns: Result<
            libloading::Symbol<unsafe extern "system" fn(*mut enginefuncs_t, *mut globalvars_t)>,
            _,
        > = unsafe { guard._lib.get(b"GiveFnptrsToDll\0") };
        match give_fns {
            Ok(f) => {
                log::trace!(target: log_targets::PROXY, "Forwarding GiveFnptrsToDll to real GameDLL...");
                unsafe { f(engfuncs, globals) };
                log::trace!(target: log_targets::PROXY, "Real GameDLL GiveFnptrsToDll returned successfully");
            }
            Err(e) => {
                log::error!(
                    target: log_targets::PROXY,
                    "ERROR: GiveFnptrsToDll not found in real GameDLL: {e}"
                );
            }
        }
    }
}

/// Helper to resolve the path of the original game DLL to proxy.
fn resolve_game_dll_path() -> PathBuf {
    let mod_dirs = [
        "cstrike", "svencoop", "valve", "czero", "dod", "tfc", "gearbox", ".",
    ];

    // 1. Try reading and parsing mod descriptor manifest (liblist.gam)
    if let Some((manifest_path, manifest)) = goldsrc_api::LibList::find_and_load(&mod_dirs) {
        log::info!(
            target: log_targets::PROXY,
            "Parsed mod manifest \"{}\": game=\"{}\" version=\"{}\" edicts={:?}",
            goldsrc_core::paths::PathResolver::normalize(&manifest_path),
            manifest.game.as_deref().unwrap_or("Unknown"),
            manifest.version.as_deref().unwrap_or("1.0"),
            manifest.edicts
        );

        if let Some(target) = manifest.target_gamedll() {
            let target_lower = target.to_ascii_lowercase();
            if !target_lower.contains("metamod") && !target_lower.contains("goldsrc") {
                let target_path = PathBuf::from(target);
                if target_path.exists() {
                    log::info!(
                        target: log_targets::PROXY,
                        "Resolved GameDLL from manifest: \"{}\"",
                        goldsrc_core::paths::PathResolver::normalize(&target_path)
                    );
                    return target_path;
                }
                if let Some(parent) = manifest_path.parent() {
                    let rel_path = parent.join(target);
                    if rel_path.exists() {
                        log::info!(
                            target: log_targets::PROXY,
                            "Resolved GameDLL relative to manifest: \"{}\"",
                            goldsrc_core::paths::PathResolver::normalize(&rel_path)
                        );
                        return rel_path;
                    }
                }
            }
        }
    }

    // 2. Search common mod directories
    let mod_dirs = [
        "cstrike", "svencoop", "valve", "czero", "dod", "tfc", "gearbox", ".",
    ];
    for mod_dir in &mod_dirs {
        for dll_name in GAME_DLL_NAMES {
            let candidate = PathBuf::from(mod_dir).join("dlls").join(dll_name);
            if candidate.exists() {
                log::info!(
                    target: log_targets::PROXY,
                    "Resolved GameDLL via mod search: \"{}\"",
                    goldsrc_core::paths::PathResolver::normalize(&candidate)
                );
                return candidate;
            }
        }
    }

    // 3. Fallback: try executable directory
    if let Ok(Some(base)) = std::env::current_exe().map(|p| p.parent().map(|d| d.to_path_buf())) {
        for mod_dir in &mod_dirs {
            for dll_name in GAME_DLL_NAMES {
                let candidate = base.join(mod_dir).join("dlls").join(dll_name);
                if candidate.exists() {
                    log::info!(
                        target: log_targets::PROXY,
                        "Resolved GameDLL via exe base search: \"{}\"",
                        goldsrc_core::paths::PathResolver::normalize(&candidate)
                    );
                    return candidate;
                }
            }
        }
    }

    PathBuf::from("cstrike")
        .join("dlls")
        .join(GAME_DLL_NAMES[0])
}

/// Load the game DLL and populate its function tables.
unsafe fn try_load_game_dll(path: &PathBuf) -> Result<GameDllProxy, Box<dyn std::error::Error>> {
    unsafe {
        // SAFETY: Loading a DLL from a trusted path.
        let lib = libloading::Library::new(path)?;

        // Retrieve the DLL_FUNCTIONS table from the real game DLL.
        let mut dll_funcs: DLL_FUNCTIONS = std::mem::zeroed();
        let mut iface_ver: i32 = 140;

        let get_entity_api2: Result<
            libloading::Symbol<unsafe extern "C" fn(*mut DLL_FUNCTIONS, *mut i32) -> i32>,
            _,
        > = lib.get(b"GetEntityAPI2\0");

        let mut loaded_api = false;
        if let Ok(f) = get_entity_api2 {
            let ret = f(&mut dll_funcs, &mut iface_ver);
            if ret != 0 {
                log::info!(
                    target: log_targets::PROXY,
                    "Successfully populated DLL_FUNCTIONS via GetEntityAPI2"
                );
                loaded_api = true;
            }
        }

        if !loaded_api {
            // Fallback: GetEntityAPI(DLL_FUNCTIONS*, int)
            let get_entity_api: Result<
                libloading::Symbol<unsafe extern "C" fn(*mut DLL_FUNCTIONS, i32) -> i32>,
                _,
            > = lib.get(b"GetEntityAPI\0");
            if let Ok(f) = get_entity_api {
                let ret = f(&mut dll_funcs, 140);
                if ret != 0 {
                    log::info!(
                        target: log_targets::PROXY,
                        "Successfully populated DLL_FUNCTIONS via GetEntityAPI"
                    );
                    loaded_api = true;
                }
            }
        }

        if !loaded_api {
            log::warn!(
                target: log_targets::PROXY,
                "WARNING: Neither GetEntityAPI2 nor GetEntityAPI returned 1 for real GameDLL!"
            );
        }

        // Retrieve NEW_DLL_FUNCTIONS if available
        let mut new_dll_funcs: NEW_DLL_FUNCTIONS = std::mem::zeroed();
        let mut new_iface_ver: i32 = 1;
        let get_new_dll_fns: Result<
            libloading::Symbol<unsafe extern "C" fn(*mut std::ffi::c_void, *mut i32) -> i32>,
            _,
        > = lib.get(b"GetNewDLLFunctions\0");

        let has_new_dll_funcs = if let Ok(f) = get_new_dll_fns {
            let ret = f(
                &mut new_dll_funcs as *mut _ as *mut std::ffi::c_void,
                &mut new_iface_ver,
            );
            if ret != 0 {
                log::info!(
                    target: log_targets::PROXY,
                    "Successfully populated NEW_DLL_FUNCTIONS via GetNewDLLFunctions"
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        // Query ReGameDLL CreateInterface factory if exported
        let create_iface: Result<
            libloading::Symbol<goldsrc_extension_reapi::c_abi::CreateInterfaceFn>,
            _,
        > = lib.get(b"CreateInterface\0");
        if let Ok(factory) = create_iface {
            goldsrc_extension_reapi::ReApiBridge::try_init_regamedll_factory(*factory);
            goldsrc_extension_reapi::register_extensions();
        }

        Ok(GameDllProxy {
            _lib: lib,
            dll_funcs,
            new_dll_funcs,
            has_new_dll_funcs,
            loaded: true,
        })
    }
}

/// Copy the real game DLL's function table into the provided table pointer.
pub fn populate_dll_table(dll_table: *mut DLL_FUNCTIONS) {
    ensure_loaded();
    if dll_table.is_null() {
        return;
    }
    if let Some(lock) = PROXY.get() {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.loaded {
            unsafe {
                std::ptr::copy_nonoverlapping(&guard.dll_funcs, dll_table, 1);
            }
            log::trace!(target: log_targets::PROXY, "Successfully copied real DLL_FUNCTIONS to engine table!");
        } else {
            log::error!(target: log_targets::PROXY, "ERROR: Real GameDLL not loaded when calling populate_dll_table!");
        }
    }
}

/// Copy NEW_DLL_FUNCTIONS from real GameDLL if present.
pub fn populate_new_dll_table(new_dll_table: *mut std::ffi::c_void) -> bool {
    ensure_loaded();
    if new_dll_table.is_null() {
        return false;
    }
    if let Some(lock) = PROXY.get() {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.loaded && guard.has_new_dll_funcs {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &guard.new_dll_funcs,
                    new_dll_table as *mut NEW_DLL_FUNCTIONS,
                    1,
                );
            }
            log::trace!(target: log_targets::PROXY, "Successfully copied real NEW_DLL_FUNCTIONS to engine table!");
            return true;
        }
    }
    false
}

/// Forward a server activate call to the real game DLL if loaded.
pub fn forward_server_activate(
    edict_list: *mut goldsrc_sys::edict_t,
    edict_count: i32,
    client_max: i32,
) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnServerActivate
    });
    if let Some(f) = func {
        unsafe { f(edict_list, edict_count, client_max) };
    }
}

/// Forward a server deactivate call to the real game DLL if loaded.
pub fn forward_server_deactivate() {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnServerDeactivate
    });
    if let Some(f) = func {
        unsafe { f() };
    }
}

/// Forward a spawn call to the real game DLL if loaded.
pub fn forward_spawn(edict: *mut goldsrc_sys::edict_t) -> i32 {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnSpawn
    });
    if let Some(f) = func {
        unsafe { f(edict) }
    } else {
        0
    }
}

/// Returns the real GameDLL `DispatchSpawn` pointer, if the DLL is loaded.
pub fn real_dispatch_spawn() -> Option<unsafe extern "C" fn(*mut goldsrc_sys::edict_t) -> i32> {
    PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.loaded {
            guard.dll_funcs.pfnSpawn
        } else {
            None
        }
    })
}

/// Returns the real GameDLL `Touch` pointer, if the DLL is loaded.
pub fn real_touch()
-> Option<unsafe extern "C" fn(*mut goldsrc_sys::edict_t, *mut goldsrc_sys::edict_t)> {
    PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.loaded {
            guard.dll_funcs.pfnTouch
        } else {
            None
        }
    })
}

/// Returns the real GameDLL `KeyValue` pointer, if the DLL is loaded.
pub fn real_key_value()
-> Option<unsafe extern "C" fn(*mut goldsrc_sys::edict_t, *mut goldsrc_sys::KeyValueData)> {
    PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.loaded {
            guard.dll_funcs.pfnKeyValue
        } else {
            None
        }
    })
}

/// Forward a client connect call to the real game DLL if loaded.
pub fn forward_client_connect(
    edict: *mut goldsrc_sys::edict_t,
    name: *const std::os::raw::c_char,
    address: *const std::os::raw::c_char,
    reject_reason: *mut std::os::raw::c_char,
) -> goldsrc_sys::qboolean {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnClientConnect
    });
    if let Some(f) = func {
        unsafe { f(edict, name, address, reject_reason) }
    } else {
        1 as goldsrc_sys::qboolean // Allow by default if no real DLL
    }
}

/// Forward a client disconnect call to the real game DLL if loaded.
pub fn forward_client_disconnect(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnClientDisconnect
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a client command call to the real game DLL if loaded.
pub fn forward_client_command(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnClientCommand
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a game init call to the real game DLL if loaded.
pub fn forward_game_init() {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnGameInit
    });
    if let Some(f) = func {
        unsafe { f() };
    }
}

/// Forward a start frame call to the real game DLL if loaded.
pub fn forward_start_frame() {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnStartFrame
    });
    if let Some(f) = func {
        unsafe { f() };
    }
}

/// Forward a player pre think call to the real game DLL if loaded.
pub fn forward_player_pre_think(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnPlayerPreThink
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a player post think call to the real game DLL if loaded.
pub fn forward_player_post_think(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnPlayerPostThink
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a cmd start call to the real game DLL if loaded.
pub fn forward_cmd_start(
    player: *const goldsrc_sys::edict_t,
    cmd: *const goldsrc_sys::usercmd_s,
    random_seed: std::os::raw::c_uint,
) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnCmdStart
    });
    if let Some(f) = func {
        unsafe { f(player, cmd, random_seed) };
    }
}

/// Forward a cmd end call to the real game DLL if loaded.
pub fn forward_cmd_end(player: *const goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnCmdEnd
    });
    if let Some(f) = func {
        unsafe { f(player) };
    }
}

/// Forward an add_to_full_pack call to the real game DLL if loaded.
pub fn forward_add_to_full_pack(
    state: *mut goldsrc_sys::entity_state_s,
    e: std::os::raw::c_int,
    ent: *mut goldsrc_sys::edict_t,
    host: *mut goldsrc_sys::edict_t,
    hostflags: std::os::raw::c_int,
    player: std::os::raw::c_int,
    pset: *mut u8,
) -> std::os::raw::c_int {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnAddToFullPack
    });
    if let Some(f) = func {
        unsafe { f(state, e, ent, host, hostflags, player, pset) }
    } else {
        -1
    }
}

/// Forward a client kill call to the real game DLL if loaded.
pub fn forward_client_kill(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnClientKill
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a touch call to the real game DLL if loaded.
pub fn forward_touch(
    pent_touched: *mut goldsrc_sys::edict_t,
    pent_other: *mut goldsrc_sys::edict_t,
) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnTouch
    });
    if let Some(f) = func {
        unsafe { f(pent_touched, pent_other) };
    }
}

/// Forward a use call to the real game DLL if loaded.
pub fn forward_use(pent_used: *mut goldsrc_sys::edict_t, pent_other: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnUse
    });
    if let Some(f) = func {
        unsafe { f(pent_used, pent_other) };
    }
}

/// Forward a client put in server call to the real game DLL if loaded.
pub fn forward_client_put_in_server(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnClientPutInServer
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a client user info changed call to the real game DLL if loaded.
pub fn forward_client_user_info_changed(
    edict: *mut goldsrc_sys::edict_t,
    infobuffer: *mut std::ffi::c_char,
) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnClientUserInfoChanged
    });
    if let Some(f) = func {
        unsafe { f(edict, infobuffer) };
    }
}

/// Forward a think call to the real game DLL if loaded.
pub fn forward_think(pent: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnThink
    });
    if let Some(f) = func {
        unsafe { f(pent) };
    }
}

/// Forward a blocked call to the real game DLL if loaded.
pub fn forward_blocked(
    pent_blocked: *mut goldsrc_sys::edict_t,
    pent_other: *mut goldsrc_sys::edict_t,
) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnBlocked
    });
    if let Some(f) = func {
        unsafe { f(pent_blocked, pent_other) };
    }
}

/// Forward a key value call to the real game DLL if loaded.
pub fn forward_key_value(pent: *mut goldsrc_sys::edict_t, pkvd: *mut goldsrc_sys::KeyValueData) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnKeyValue
    });
    if let Some(f) = func {
        unsafe { f(pent, pkvd) };
    }
}

/// Forward a set abs box call to the real game DLL if loaded.
pub fn forward_set_abs_box(pent: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnSetAbsBox
    });
    if let Some(f) = func {
        unsafe { f(pent) };
    }
}

/// Forward an update client data call to the real game DLL if loaded.
pub fn forward_update_client_data(
    ent: *const goldsrc_sys::edict_t,
    sendweapons: std::os::raw::c_int,
    cd: *mut goldsrc_sys::clientdata_s,
) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnUpdateClientData
    });
    if let Some(f) = func {
        unsafe { f(ent, sendweapons, cd) };
    }
}

/// Forward a spectator connect call to the real game DLL if loaded.
pub fn forward_spectator_connect(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnSpectatorConnect
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a spectator disconnect call to the real game DLL if loaded.
pub fn forward_spectator_disconnect(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnSpectatorDisconnect
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a spectator think call to the real game DLL if loaded.
pub fn forward_spectator_think(edict: *mut goldsrc_sys::edict_t) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnSpectatorThink
    });
    if let Some(f) = func {
        unsafe { f(edict) };
    }
}

/// Forward a sys error call to the real game DLL if loaded.
pub fn forward_sys_error(error_string: *const std::ffi::c_char) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnSys_Error
    });
    if let Some(f) = func {
        unsafe { f(error_string) };
    }
}

/// Forward a pm move call to the real game DLL if loaded.
pub fn forward_pm_move(ppmove: *mut goldsrc_sys::playermove_s, server: goldsrc_sys::qboolean) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnPM_Move
    });
    if let Some(f) = func {
        unsafe { f(ppmove, server) };
    }
}

/// Forward a setup visibility call to the real game DLL if loaded.
pub fn forward_setup_visibility(
    view_entity: *mut goldsrc_sys::edict_t,
    client: *mut goldsrc_sys::edict_t,
    pvs: *mut *mut u8,
    pas: *mut *mut u8,
) {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnSetupVisibility
    });
    if let Some(f) = func {
        unsafe { f(view_entity, client, pvs, pas) };
    }
}

/// Forward an inconsistent file call to the real game DLL if loaded.
pub fn forward_inconsistent_file(
    player: *const goldsrc_sys::edict_t,
    filename: *const std::ffi::c_char,
    disconnect_message: *mut std::ffi::c_char,
) -> std::os::raw::c_int {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnInconsistentFile
    });
    if let Some(f) = func {
        unsafe { f(player, filename, disconnect_message) }
    } else {
        0
    }
}

/// Forward an allow lag compensation call to the real game DLL if loaded.
pub fn forward_allow_lag_compensation() -> std::os::raw::c_int {
    let func = PROXY.get().and_then(|lock| {
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        guard.dll_funcs.pfnAllowLagCompensation
    });
    if let Some(f) = func {
        unsafe { f() }
    } else {
        1
    }
}

/// Forward Server_GetBlendingInterface call to the real game DLL.
///
/// # Safety
/// Pointers must be valid or null as accepted by HLSDK.
pub unsafe fn forward_server_get_blending_interface(
    version: i32,
    ppinterface: *mut *mut std::ffi::c_void,
    pstudio: *mut std::ffi::c_void,
    rotationmatrix: *mut std::ffi::c_void,
    bonetransform: *mut std::ffi::c_void,
) -> i32 {
    ensure_loaded();
    let func = PROXY.get().and_then(|proxy_lock| {
        let guard = proxy_lock.lock().unwrap_or_else(|e| e.into_inner());
        let func: Result<
            libloading::Symbol<
                unsafe extern "C" fn(
                    i32,
                    *mut *mut std::ffi::c_void,
                    *mut std::ffi::c_void,
                    *mut std::ffi::c_void,
                    *mut std::ffi::c_void,
                ) -> i32,
            >,
            _,
        > = unsafe { guard._lib.get(b"Server_GetBlendingInterface\0") };
        func.ok().map(|sym| *sym)
    });
    if let Some(f) = func {
        unsafe { f(version, ppinterface, pstudio, rotationmatrix, bonetransform) }
    } else {
        0
    }
}

/// Forward an entity factory function call (e.g. worldspawn, info_player_start) to the real game DLL.
///
/// # Safety
/// `pev` is a valid pointer to an `entvars_t` allocated by the engine.
pub unsafe fn forward_entity(name: &str, pev: *mut goldsrc_sys::entvars_t) {
    ensure_loaded();
    let func = PROXY.get().and_then(|proxy_lock| {
        let guard = proxy_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut name_bytes = name.as_bytes().to_vec();
        name_bytes.push(0);
        let func: Result<libloading::Symbol<unsafe extern "C" fn(*mut goldsrc_sys::entvars_t)>, _> =
            unsafe { guard._lib.get(&name_bytes[..]) };
        func.ok().map(|sym| *sym)
    });
    if let Some(f) = func {
        unsafe { f(pev) };
    }
}

/// Forward CreateInterface call to the real game DLL if available.
///
/// # Safety
/// `name` must be a valid null-terminated C string.
pub unsafe fn forward_create_interface(
    name: *const std::os::raw::c_char,
    return_code: *mut i32,
) -> *mut std::ffi::c_void {
    ensure_loaded();
    let func = PROXY.get().and_then(|proxy_lock| {
        let guard = proxy_lock.lock().unwrap_or_else(|e| e.into_inner());
        let func: Result<
            libloading::Symbol<
                unsafe extern "C" fn(
                    *const std::os::raw::c_char,
                    *mut i32,
                ) -> *mut std::ffi::c_void,
            >,
            _,
        > = unsafe { guard._lib.get(b"CreateInterface\0") };
        func.ok().map(|sym| *sym)
    });
    if let Some(f) = func {
        unsafe { f(name, return_code) }
    } else {
        std::ptr::null_mut()
    }
}
