//! Unified FFI registration point for engine-facing function tables.
//!
//! This module is the single place where `DLL_FUNCTIONS` hook tables are
//! populated, replacing per-backend hand-written table filling. It separates
//! three concerns:
//!
//! 1. **FFI boundary** — `extern "C"` trampolines + panic guards (here).
//! 2. **Engine interaction** — edict→index resolution, client command reading.
//! 3. **Backend strategy** — [`EntityHooks`] impl per backend (forward to real
//!    GameDLL, metamod MRES handling, event emission, ...).
//!
//! Backends implement [`EntityHooks`] once, register it via [`register`],
//! and let [`install_dll_api2`] / [`install_dll_api2_post`] fill the tables.

use goldsrc_api::consts::log_targets;
use goldsrc_sys::{DLL_FUNCTIONS, edict_t, enginefuncs_t, qboolean};
use std::ffi::c_char;
use std::os::raw::c_int;
use std::sync::OnceLock;

use goldsrc_sys::ffi::catch_ffi_panic;

/// Backend-specific behavior for each hooked `DLL_FUNCTIONS` slot.
///
/// All methods default to no-ops; backends override only what they need.
/// Business logic (WASM events / command dispatch) is invoked explicitly by
/// each impl through [`crate::hooks`], keeping this trait free of hidden magic.
///
/// Raw pointers are forwarded as-is because backends may need to pass them to
/// the real GameDLL or metamod; precomputed indexes/strings save impls from
/// repeating engine reads.
#[allow(unused_variables)]
pub trait EntityHooks: Send + Sync {
    // --- DLL_FUNCTIONS (pre-hooks / direct GameDLL replacement) ---

    /// `pfnGameInit` — server startup.
    fn game_init(&self) {}
    /// `pfnSpawn` — entity spawn. Returns the value passed back to the engine.
    fn spawn(&self, edict: *mut edict_t) -> i32 {
        0
    }
    /// `pfnServerActivate` — map loaded and activated.
    fn server_activate(&self, edict_list: *mut edict_t, edict_count: i32, client_max: i32) {}
    /// `pfnServerDeactivate` — map ending / shutdown.
    fn server_deactivate(&self) {}
    /// `pfnClientConnect`. Returns the connect verdict (1 = allow, 0 = reject).
    fn client_connect(
        &self,
        edict: *mut edict_t,
        index: i32,
        name: *const c_char,
        address: *const c_char,
        reject_reason: *mut c_char,
    ) -> i32 {
        1
    }
    /// `pfnClientDisconnect`.
    fn client_disconnect(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnClientCommand`. Returns `true` if a plugin consumed the command
    /// (backend decides how to suppress the GameDLL, e.g. proxy skip or MRES).
    fn client_command(&self, edict: *mut edict_t, index: i32, cmd: &str, args: &str) -> bool {
        false
    }
    /// `pfnStartFrame` — every server frame.
    fn start_frame(&self) {}
    /// `pfnPlayerPreThink` — pre-physics/movement player tick.
    fn player_pre_think(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnPlayerPostThink` — post-physics player tick.
    fn player_post_think(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnClientKill` — suicide command.
    fn client_kill(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnTouch` — two entities collided.
    fn touch(&self, touched: *mut edict_t, touched_idx: i32, other: *mut edict_t, other_idx: i32) {}
    /// `pfnUse` — entity activated/triggered.
    fn entity_use(&self, used: *mut edict_t, used_idx: i32, other: *mut edict_t, other_idx: i32) {}
    /// `pfnCmdStart` — client usercmd processing start.
    fn cmd_start(
        &self,
        player: *const edict_t,
        index: i32,
        cmd: *const goldsrc_sys::usercmd_s,
        random_seed: u32,
    ) {
    }
    /// `pfnCmdEnd` — client usercmd processing end.
    fn cmd_end(&self, player: *const edict_t, index: i32) {}
    /// `pfnAddToFullPack` — per-client entity state packet customization.
    #[allow(clippy::too_many_arguments)]
    fn add_to_full_pack(
        &self,
        state: *mut goldsrc_sys::entity_state_s,
        e: i32,
        ent: *mut edict_t,
        host: *mut edict_t,
        hostflags: i32,
        player: i32,
        pset: *mut u8,
    ) -> i32 {
        -1
    }

    /// `pfnClientUserInfoChanged` — client userinfo modified.
    fn client_user_info_changed(&self, edict: *mut edict_t, index: i32, infobuffer: *mut c_char) {}
    /// `pfnClientPutInServer` — player fully enters game world.
    fn client_put_in_server(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnThink` — entity think callback.
    fn think(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnBlocked` — entity blocked by other entity.
    fn blocked(
        &self,
        blocked: *mut edict_t,
        blocked_idx: i32,
        other: *mut edict_t,
        other_idx: i32,
    ) {
    }
    /// `pfnKeyValue` — entity key-value dispatch.
    fn key_value(&self, edict: *mut edict_t, index: i32, pkvd: *mut goldsrc_sys::KeyValueData) {}
    /// `pfnSetAbsBox` — entity bounding box update.
    fn set_abs_box(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnUpdateClientData` — client HUD & weapon data refresh.
    fn update_client_data(
        &self,
        edict: *const edict_t,
        index: i32,
        sendweapons: i32,
        cd: *mut goldsrc_sys::clientdata_s,
    ) {
    }
    /// `pfnSpectatorConnect` — spectator connected.
    fn spectator_connect(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnSpectatorDisconnect` — spectator disconnected.
    fn spectator_disconnect(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnSpectatorThink` — spectator think tick.
    fn spectator_think(&self, edict: *mut edict_t, index: i32) {}
    /// `pfnSys_Error` — engine fatal error shutdown notice.
    fn sys_error(&self, error_string: &str) {}
    /// `pfnPM_Move` — custom player movement execution.
    fn pm_move(&self, ppmove: *mut goldsrc_sys::playermove_s, server: qboolean) {}
    /// `pfnSetupVisibility` — custom PVS/PAS calculation.
    fn setup_visibility(
        &self,
        view_ent: *mut edict_t,
        client: *mut edict_t,
        pvs: *mut *mut u8,
        pas: *mut *mut u8,
    ) {
    }
    /// `pfnInconsistentFile` — consistency check failed for client file.
    fn inconsistent_file(
        &self,
        player: *const edict_t,
        index: i32,
        filename: &str,
        disconnect_message: *mut c_char,
    ) -> i32 {
        0
    }
    /// `pfnAllowLagCompensation` — server lag compensation check.
    fn allow_lag_compensation(&self) -> i32 {
        1
    }

    // --- DLL_FUNCTIONS (post-hooks; used by metamod chaining) ---

    /// Post-`pfnSpawn`.
    fn spawn_post(&self, edict: *mut edict_t) {}
    /// Post-`pfnClientConnect`.
    fn client_connect_post(&self, index: i32) {}
    /// Post-`pfnClientDisconnect`.
    fn client_disconnect_post(&self, index: i32) {}
    /// Post-`pfnClientPutInServer`.
    fn client_put_in_server_post(&self, index: i32) {}
    /// Post-`pfnClientUserInfoChanged`.
    fn client_user_info_changed_post(&self, index: i32) {}
    /// Post-`pfnStartFrame`.
    fn start_frame_post(&self) {}
    /// Post-`pfnPlayerPreThink`.
    fn player_pre_think_post(&self, index: i32) {}
    /// Post-`pfnPlayerPostThink`.
    fn player_post_think_post(&self, index: i32) {}
    /// Post-`pfnUpdateClientData`.
    fn update_client_data_post(&self, index: i32) {}
}

/// Everything a backend provides so trampolines can resolve engine state.
pub struct Registry {
    pub hooks: &'static dyn EntityHooks,
    /// Engine func-table accessor (same one handed to `EngineBackend::new`).
    pub engfuncs: fn() -> &'static enginefuncs_t,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static WARNED_UNREGISTERED: OnceLock<()> = OnceLock::new();

/// Registers the backend hook implementation. Idempotent: first call wins.
pub fn register(config: Registry) {
    let _ = REGISTRY.set(config);
}

fn hooks() -> Option<&'static dyn EntityHooks> {
    match REGISTRY.get() {
        Some(r) => Some(r.hooks),
        // Silent no-op here would make an unregistered backend look like a
        // plugin bug ("events don't fire") — surface the real cause once.
        None => {
            if WARNED_UNREGISTERED.set(()).is_ok() {
                log::warn!(target: log_targets::CORE,
                    "api_registry: hook fired before register() — event dropped");
            }
            None
        }
    }
}

fn engfuncs() -> Option<&'static enginefuncs_t> {
    REGISTRY.get().map(|r| (r.engfuncs)())
}

/// HLSDK interface-version negotiation for `GetEntityAPI2`-style entry points.
///
/// Writes our supported version back so the engine can report the mismatch.
/// With `strict = true` (HLSDK-correct policy, used by metamod) a mismatch
/// also aborts registration; with `strict = false` (standalone proxy, which
/// overlays its own table anyway) only the pointer write is performed.
///
/// # Safety
/// `interface_version` must be null or point to a valid `i32` provided by the
/// engine for version exchange.
pub unsafe fn negotiate_interface_version(interface_version: *mut i32, strict: bool) -> bool {
    if interface_version.is_null() {
        return !strict;
    }
    // SAFETY: non-null pointer provided by the engine for version exchange.
    unsafe {
        if *interface_version != goldsrc_api::consts::ENGINE_INTERFACE_VERSION {
            *interface_version = goldsrc_api::consts::ENGINE_INTERFACE_VERSION;
            return !strict;
        }
    }
    true
}

static EDICT_BASE: std::sync::atomic::AtomicPtr<edict_t> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static EDICT_COUNT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Updates the known active edict array boundaries from `server_activate`.
pub fn update_edict_bounds(base: *mut edict_t, count: i32) {
    EDICT_BASE.store(base, std::sync::atomic::Ordering::Relaxed);
    EDICT_COUNT.store(count, std::sync::atomic::Ordering::Relaxed);
}

/// Resolves an edict pointer to its entity index safely (0 if unresolvable or out-of-bounds).
///
/// # Safety
/// `edict` must be null or a valid edict pointer provided by the engine.
pub unsafe fn edict_index(edict: *mut edict_t) -> i32 {
    if edict.is_null() {
        return 0;
    }
    let base = EDICT_BASE.load(std::sync::atomic::Ordering::Relaxed);
    let count = EDICT_COUNT.load(std::sync::atomic::Ordering::Relaxed);
    if !base.is_null() && count > 0 {
        let diff = (edict as usize).wrapping_sub(base as usize);
        let edict_size = std::mem::size_of::<edict_t>();
        if edict_size > 0 && diff.is_multiple_of(edict_size) {
            let idx = (diff / edict_size) as i32;
            if idx >= 0 && idx < count {
                return idx;
            }
        }
    }

    // Fallback: query engine's pfnIndexOfEdict if available
    if let Some(funcs) = engfuncs()
        && let Some(index_fn) = funcs.pfnIndexOfEdict
    {
        return unsafe { index_fn(edict) };
    }

    0
}

/// Reads the currently dispatched client command as `(cmd, args)`.
pub fn read_client_command() -> (String, String) {
    let Some(funcs) = engfuncs() else {
        return (String::new(), String::new());
    };
    // SAFETY: engine-provided C strings, valid during ClientCommand dispatch.
    unsafe {
        let cmd = crate::backend::cstr_to_string(
            funcs.pfnCmd_Argv.map(|f| f(0)).unwrap_or(std::ptr::null()),
        );
        let args = crate::backend::cstr_to_string(
            funcs.pfnCmd_Args.map(|f| f()).unwrap_or(std::ptr::null()),
        );
        (cmd, args)
    }
}

/// Packs two entity indexes into an 8-byte little-endian event payload.
pub fn pack_two_i32(a: i32, b: i32) -> [u8; 8] {
    let mut payload = [0u8; 8];
    payload[0..4].copy_from_slice(&a.to_le_bytes());
    payload[4..8].copy_from_slice(&b.to_le_bytes());
    payload
}

// ============================================================================
// Trampolines — the only `extern "C"` hook bodies in the codebase.
// ============================================================================

/// # Safety
/// Engine callback; invoked only through the installed function tables.
pub unsafe extern "C" fn api_game_init() {
    catch_ffi_panic("game_init", (), || {
        if let Some(h) = hooks() {
            h.game_init();
        }
    });
}

/// # Safety
/// Engine callback; `pent` must be a valid edict pointer or null.
pub unsafe extern "C" fn api_spawn(pent: *mut edict_t) -> c_int {
    catch_ffi_panic("spawn", 0, || hooks().map_or(0, |h| h.spawn(pent)))
}

/// # Safety
/// Engine callback; `p_edict_list` must be a valid edict array base.
pub unsafe extern "C" fn api_server_activate(
    p_edict_list: *mut edict_t,
    edict_count: c_int,
    client_max: c_int,
) {
    catch_ffi_panic("server_activate", (), || {
        update_edict_bounds(p_edict_list, edict_count);
        if let Some(h) = hooks() {
            h.server_activate(p_edict_list, edict_count, client_max);
        }
    });
}

/// # Safety
/// Engine callback; invoked only through the installed function tables.
pub unsafe extern "C" fn api_server_deactivate() {
    catch_ffi_panic("server_deactivate", (), || {
        if let Some(h) = hooks() {
            h.server_deactivate();
        }
    });
}

/// # Safety
/// Engine callback; string pointers must be valid C strings per HLSDK contract.
pub unsafe extern "C" fn api_client_connect(
    p_entity: *mut edict_t,
    psz_name: *const c_char,
    psz_address: *const c_char,
    sz_reject_reason: *mut c_char,
) -> qboolean {
    catch_ffi_panic("client_connect", 0 as qboolean, || {
        let index = unsafe { edict_index(p_entity) };
        hooks().map_or(0 as qboolean, |h| {
            h.client_connect(p_entity, index, psz_name, psz_address, sz_reject_reason) as qboolean
        })
    })
}

/// # Safety
/// Engine callback; `p_entity` must be a valid player edict pointer or null.
pub unsafe extern "C" fn api_client_disconnect(p_entity: *mut edict_t) {
    catch_ffi_panic("client_disconnect", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.client_disconnect(p_entity, index);
        }
    });
}

/// # Safety
/// Engine callback; `p_entity` must be a valid edict pointer; command buffer
/// (`pfnCmd_Argv`/`pfnCmd_Args`) must be active.
pub unsafe extern "C" fn api_client_command(p_entity: *mut edict_t) {
    catch_ffi_panic("client_command", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            let (cmd, args) = read_client_command();
            h.client_command(p_entity, index, &cmd, &args);
        }
    });
}

/// # Safety
/// Engine callback; invoked only through the installed function tables.
pub unsafe extern "C" fn api_start_frame() {
    catch_ffi_panic("start_frame", (), || {
        if let Some(h) = hooks() {
            h.start_frame();
        }
    });
}

/// # Safety
/// Engine callback; `p_entity` must be a valid player edict pointer.
pub unsafe extern "C" fn api_player_pre_think(p_entity: *mut edict_t) {
    catch_ffi_panic("player_pre_think", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.player_pre_think(p_entity, index);
        }
    });
}

/// # Safety
/// Engine callback; `p_entity` must be a valid player edict pointer.
pub unsafe extern "C" fn api_player_post_think(p_entity: *mut edict_t) {
    catch_ffi_panic("player_post_think", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.player_post_think(p_entity, index);
        }
    });
}

/// # Safety
/// Engine callback; `p_entity` must be a valid player edict pointer.
pub unsafe extern "C" fn api_client_kill(p_entity: *mut edict_t) {
    catch_ffi_panic("client_kill", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.client_kill(p_entity, index);
        }
    });
}

/// # Safety
/// Engine callback; both pointers must be valid edicts or null.
pub unsafe extern "C" fn api_touch(pent_touched: *mut edict_t, pent_other: *mut edict_t) {
    catch_ffi_panic("touch", (), || {
        if let Some(h) = hooks() {
            let touched_idx = unsafe { edict_index(pent_touched) };
            let other_idx = unsafe { edict_index(pent_other) };
            h.touch(pent_touched, touched_idx, pent_other, other_idx);
        }
    });
}

/// # Safety
/// Engine callback; both pointers must be valid edicts or null.
pub unsafe extern "C" fn api_use(pent_used: *mut edict_t, pent_other: *mut edict_t) {
    catch_ffi_panic("use", (), || {
        if let Some(h) = hooks() {
            let used_idx = unsafe { edict_index(pent_used) };
            let other_idx = unsafe { edict_index(pent_other) };
            h.entity_use(pent_used, used_idx, pent_other, other_idx);
        }
    });
}

/// # Safety
/// Engine callback; `player` must be a valid player edict pointer; `cmd` must point to valid `usercmd_s`.
pub unsafe extern "C" fn api_cmd_start(
    player: *const edict_t,
    cmd: *const goldsrc_sys::usercmd_s,
    random_seed: std::os::raw::c_uint,
) {
    catch_ffi_panic("cmd_start", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(player as *mut edict_t) };
            h.cmd_start(player, index, cmd, random_seed);
        }
    });
}

/// # Safety
/// Engine callback; `player` must be a valid player edict pointer.
pub unsafe extern "C" fn api_cmd_end(player: *const edict_t) {
    catch_ffi_panic("cmd_end", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(player as *mut edict_t) };
            h.cmd_end(player, index);
        }
    });
}

/// # Safety
/// Engine callback; pointers must conform to HLSDK `AddToFullPack` contract.
pub unsafe extern "C" fn api_add_to_full_pack(
    state: *mut goldsrc_sys::entity_state_s,
    e: c_int,
    ent: *mut edict_t,
    host: *mut edict_t,
    hostflags: c_int,
    player: c_int,
    pset: *mut u8,
) -> c_int {
    catch_ffi_panic("add_to_full_pack", -1, || {
        hooks().map_or(-1, |h| {
            h.add_to_full_pack(state, e, ent, host, hostflags, player, pset)
        })
    })
}

// --- Post trampolines (metamod chaining) ---

/// # Safety
/// Metamod post-hook callback; `pent` must be a valid edict pointer.
pub unsafe extern "C" fn api_spawn_post(pent: *mut edict_t) -> c_int {
    catch_ffi_panic("spawn_post", 0, || {
        if let Some(h) = hooks() {
            h.spawn_post(pent);
        }
        let index = unsafe { edict_index(pent) };
        if index >= 0 {
            let _ = crate::hooks::vtable::VTableBridge::dispatch_generic(
                goldsrc_api::gamedata::VTableFunc::Spawn,
                index,
                crate::hooks::HookTiming::Post,
            );
        }
        0
    })
}

/// # Safety
/// Metamod post-hook callback; `p_entity` must be a valid edict pointer.
pub unsafe extern "C" fn api_client_connect_post(
    p_entity: *mut edict_t,
    _psz_name: *const c_char,
    _psz_address: *const c_char,
    _sz_reject_reason: *mut c_char,
) -> qboolean {
    catch_ffi_panic("client_connect_post", 1, || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.client_connect_post(index);
        }
        1
    })
}

/// # Safety
/// Metamod post-hook callback; `p_entity` must be a valid edict pointer.
pub unsafe extern "C" fn api_client_disconnect_post(p_entity: *mut edict_t) {
    catch_ffi_panic("client_disconnect_post", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.client_disconnect_post(index);
        }
    });
}

/// # Safety
/// Metamod post-hook callback; `p_entity` must be a valid edict pointer.
pub unsafe extern "C" fn api_client_user_info_changed_post(
    p_entity: *mut edict_t,
    _infobuffer: *mut c_char,
) {
    catch_ffi_panic("client_user_info_changed_post", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.client_user_info_changed_post(index);
        }
    });
}

/// # Safety
/// Metamod post-hook callback; invoked only through the installed tables.
pub unsafe extern "C" fn api_start_frame_post() {
    catch_ffi_panic("start_frame_post", (), || {
        if let Some(h) = hooks() {
            h.start_frame_post();
        }
    });
}

/// # Safety
/// Metamod post-hook callback; `p_entity` must be a valid player edict pointer.
pub unsafe extern "C" fn api_player_pre_think_post(p_entity: *mut edict_t) {
    catch_ffi_panic("player_pre_think_post", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.player_pre_think_post(index);
        }
    });
}

/// # Safety
/// Metamod post-hook callback; `p_entity` must be a valid player edict pointer.
pub unsafe extern "C" fn api_player_post_think_post(p_entity: *mut edict_t) {
    catch_ffi_panic("player_post_think_post", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.player_post_think_post(index);
        }
    });
}

// ============================================================================
// Table installers & Unified ApiRegistry Facade.
// ============================================================================

/// Hook phase defining which table slots (pre-hooks or post-hooks) to populate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    /// Pre-hook / GameDLL replacement table (GetEntityAPI / GetEntityAPI2).
    Pre,
    /// Post-hook / Metamod chaining table (GetEntityAPI_Post / GetEntityAPI2_Post).
    Post,
}

/// Unified API registration facade for backends.
pub struct ApiRegistry;

impl ApiRegistry {
    /// Registers the backend hook implementation. Idempotent: first call wins.
    pub fn register(hooks: &'static dyn EntityHooks, engfuncs: fn() -> &'static enginefuncs_t) {
        register(Registry { hooks, engfuncs });
    }

    /// Negotiates the interface version and installs the corresponding hook trampolines.
    ///
    /// # Safety
    /// `table` and `interface_version` must be valid non-null pointers provided by the engine/metamod.
    pub unsafe fn install(
        table: *mut DLL_FUNCTIONS,
        interface_version: *mut i32,
        phase: HookPhase,
        strict: bool,
    ) -> bool {
        if table.is_null() {
            return false;
        }
        // SAFETY: caller guarantees interface_version validity.
        if !unsafe { negotiate_interface_version(interface_version, strict) } {
            return false;
        }
        // SAFETY: caller guarantees table validity.
        unsafe {
            match phase {
                HookPhase::Pre => install_dll_api2(table),
                HookPhase::Post => install_dll_api2_post(table),
            }
        }
        true
    }
}

/// # Safety
/// Engine callback; `p_entity` must be a valid edict pointer.
pub unsafe extern "C" fn api_client_put_in_server(p_entity: *mut edict_t) {
    catch_ffi_panic("client_put_in_server", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.client_put_in_server(p_entity, index);
        }
        let index = unsafe { edict_index(p_entity) };
        if goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            crate::hooks::dispatcher::emit(crate::host::HostEvent::Player {
                slot: index,
                event: crate::host::PlayerEvent::PutInServer,
            });
        }
    });
}

/// # Safety
/// Engine callback; `p_entity` must be a valid edict pointer.
pub unsafe extern "C" fn api_client_user_info_changed(
    p_entity: *mut edict_t,
    infobuffer: *mut c_char,
) {
    catch_ffi_panic("client_user_info_changed", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.client_user_info_changed(p_entity, index, infobuffer);
        }
        let index = unsafe { edict_index(p_entity) };
        if goldsrc_api::PlayerSlot::VALID_RANGE.contains(&index) {
            crate::hooks::dispatcher::emit(crate::host::HostEvent::Player {
                slot: index,
                event: crate::host::PlayerEvent::UserInfoChanged,
            });
        }
    });
}

/// # Safety
/// Engine callback; `pent` must be a valid edict pointer.
pub unsafe extern "C" fn api_think(pent: *mut edict_t) {
    catch_ffi_panic("think", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(pent) };
            h.think(pent, index);
        }
    });
}

/// # Safety
/// Engine callback; pointers must be valid edicts or null.
pub unsafe extern "C" fn api_blocked(pent_blocked: *mut edict_t, pent_other: *mut edict_t) {
    catch_ffi_panic("blocked", (), || {
        if let Some(h) = hooks() {
            let b_idx = unsafe { edict_index(pent_blocked) };
            let o_idx = unsafe { edict_index(pent_other) };
            h.blocked(pent_blocked, b_idx, pent_other, o_idx);
        }
    });
}

/// # Safety
/// Engine callback; pointers must conform to HLSDK `KeyValueData` contract.
pub unsafe extern "C" fn api_key_value(pent: *mut edict_t, pkvd: *mut goldsrc_sys::KeyValueData) {
    catch_ffi_panic("key_value", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(pent) };
            h.key_value(pent, index, pkvd);
        }
    });
}

/// # Safety
/// Engine callback; `pent` must be a valid edict pointer.
pub unsafe extern "C" fn api_set_abs_box(pent: *mut edict_t) {
    catch_ffi_panic("set_abs_box", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(pent) };
            h.set_abs_box(pent, index);
        }
    });
}

/// # Safety
/// Engine callback; `ent` must be a valid edict pointer, `cd` points to clientdata struct.
pub unsafe extern "C" fn api_update_client_data(
    ent: *const edict_t,
    sendweapons: c_int,
    cd: *mut goldsrc_sys::clientdata_s,
) {
    catch_ffi_panic("update_client_data", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(ent as *mut edict_t) };
            h.update_client_data(ent, index, sendweapons, cd);
        }
    });
}

/// # Safety
/// Metamod post-hook callback for `update_client_data`.
pub unsafe extern "C" fn api_update_client_data_post(
    ent: *const edict_t,
    _sendweapons: c_int,
    _cd: *mut goldsrc_sys::clientdata_s,
) {
    catch_ffi_panic("update_client_data_post", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(ent as *mut edict_t) };
            h.update_client_data_post(index);
        }
    });
}

/// # Safety
/// Metamod post-hook callback for `client_put_in_server`.
pub unsafe extern "C" fn api_client_put_in_server_post(p_entity: *mut edict_t) {
    catch_ffi_panic("client_put_in_server_post", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.client_put_in_server_post(index);
        }
    });
}

/// # Safety
/// Engine callback; `p_entity` must be a valid edict pointer.
pub unsafe extern "C" fn api_spectator_connect(p_entity: *mut edict_t) {
    catch_ffi_panic("spectator_connect", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.spectator_connect(p_entity, index);
        }
    });
}

/// # Safety
/// Engine callback; `p_entity` must be a valid edict pointer.
pub unsafe extern "C" fn api_spectator_disconnect(p_entity: *mut edict_t) {
    catch_ffi_panic("spectator_disconnect", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.spectator_disconnect(p_entity, index);
        }
    });
}

/// # Safety
/// Engine callback; `p_entity` must be a valid edict pointer.
pub unsafe extern "C" fn api_spectator_think(p_entity: *mut edict_t) {
    catch_ffi_panic("spectator_think", (), || {
        if let Some(h) = hooks() {
            let index = unsafe { edict_index(p_entity) };
            h.spectator_think(p_entity, index);
        }
    });
}

/// # Safety
/// Engine callback; `error_string` is a null-terminated C string.
pub unsafe extern "C" fn api_sys_error(error_string: *const c_char) {
    catch_ffi_panic("sys_error", (), || {
        let msg = unsafe { crate::backend::cstr_to_string(error_string) };
        log::error!(target: log_targets::CORE, "Engine Sys_Error: {msg}");
        if let Some(h) = hooks() {
            h.sys_error(&msg);
        }
    });
}

/// # Safety
/// Engine callback; `ppmove` points to playermove struct.
pub unsafe extern "C" fn api_pm_move(ppmove: *mut goldsrc_sys::playermove_s, server: qboolean) {
    catch_ffi_panic("pm_move", (), || {
        if let Some(h) = hooks() {
            h.pm_move(ppmove, server);
        }
    });
}

/// # Safety
/// Engine callback for visibility PVS/PAS setup.
pub unsafe extern "C" fn api_setup_visibility(
    view_entity: *mut edict_t,
    client: *mut edict_t,
    pvs: *mut *mut u8,
    pas: *mut *mut u8,
) {
    catch_ffi_panic("setup_visibility", (), || {
        if let Some(h) = hooks() {
            h.setup_visibility(view_entity, client, pvs, pas);
        }
    });
}

/// # Safety
/// Engine callback for file consistency validation.
pub unsafe extern "C" fn api_inconsistent_file(
    player: *const edict_t,
    filename: *const c_char,
    disconnect_message: *mut c_char,
) -> c_int {
    catch_ffi_panic("inconsistent_file", 0, || {
        let name = unsafe { crate::backend::cstr_to_string(filename) };
        let index = unsafe { edict_index(player as *mut edict_t) };
        hooks().map_or(0, |h| {
            h.inconsistent_file(player, index, &name, disconnect_message)
        })
    })
}

/// # Safety
/// Engine callback for lag compensation check.
pub unsafe extern "C" fn api_allow_lag_compensation() -> c_int {
    catch_ffi_panic("allow_lag_compensation", 1, || {
        hooks().map_or(1, |h| h.allow_lag_compensation())
    })
}

/// Fills a `DLL_FUNCTIONS` table with all pre-hook trampolines.
///
/// # Safety
/// `table` must point to a writable `DLL_FUNCTIONS` owned by the caller.
pub unsafe fn install_dll_api2(table: *mut DLL_FUNCTIONS) {
    // SAFETY: caller guarantees table validity.
    unsafe {
        let t = &mut *table;
        t.pfnGameInit = Some(api_game_init);
        t.pfnSpawn = Some(api_spawn);
        t.pfnThink = Some(api_think);
        t.pfnUse = Some(api_use);
        t.pfnTouch = Some(api_touch);
        t.pfnBlocked = Some(api_blocked);
        t.pfnKeyValue = Some(api_key_value);
        t.pfnSetAbsBox = Some(api_set_abs_box);
        t.pfnClientConnect = Some(api_client_connect);
        t.pfnClientDisconnect = Some(api_client_disconnect);
        t.pfnClientKill = Some(api_client_kill);
        t.pfnClientPutInServer = Some(api_client_put_in_server);
        t.pfnClientCommand = Some(api_client_command);
        t.pfnClientUserInfoChanged = Some(api_client_user_info_changed);
        t.pfnServerActivate = Some(api_server_activate);
        t.pfnServerDeactivate = Some(api_server_deactivate);
        t.pfnPlayerPreThink = Some(api_player_pre_think);
        t.pfnPlayerPostThink = Some(api_player_post_think);
        t.pfnStartFrame = Some(api_start_frame);
        t.pfnSpectatorConnect = Some(api_spectator_connect);
        t.pfnSpectatorDisconnect = Some(api_spectator_disconnect);
        t.pfnSpectatorThink = Some(api_spectator_think);
        t.pfnSys_Error = Some(api_sys_error);
        t.pfnPM_Move = Some(api_pm_move);
        t.pfnSetupVisibility = Some(api_setup_visibility);
        t.pfnUpdateClientData = Some(api_update_client_data);
        t.pfnAddToFullPack = Some(api_add_to_full_pack);
        t.pfnCmdStart = Some(api_cmd_start);
        t.pfnCmdEnd = Some(api_cmd_end);
        t.pfnInconsistentFile = Some(api_inconsistent_file);
        t.pfnAllowLagCompensation = Some(api_allow_lag_compensation);
    }
}

/// Fills a `DLL_FUNCTIONS` table with all post-hook trampolines.
///
/// # Safety
/// `table` must point to a writable `DLL_FUNCTIONS` owned by the caller.
pub unsafe fn install_dll_api2_post(table: *mut DLL_FUNCTIONS) {
    // SAFETY: caller guarantees table validity.
    unsafe {
        let t = &mut *table;
        t.pfnSpawn = Some(api_spawn_post);
        t.pfnServerActivate = Some(api_server_activate);
        t.pfnClientConnect = Some(api_client_connect_post);
        t.pfnClientDisconnect = Some(api_client_disconnect_post);
        t.pfnClientPutInServer = Some(api_client_put_in_server_post);
        t.pfnClientUserInfoChanged = Some(api_client_user_info_changed_post);
        t.pfnStartFrame = Some(api_start_frame_post);
        t.pfnPlayerPreThink = Some(api_player_pre_think_post);
        t.pfnPlayerPostThink = Some(api_player_post_think_post);
        t.pfnUpdateClientData = Some(api_update_client_data_post);
    }
}
