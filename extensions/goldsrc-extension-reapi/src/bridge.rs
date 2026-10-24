//! Dynamic runtime discovery and bridge for ReHLDS and ReGameDLL.
//!
//! Provides zero-overhead access to ReAPI extensions, queries `CreateInterface`
//! dynamically on module load, and verifies major/minor version compatibility.

use crate::c_abi::{
    CreateInterfaceFn, IReGameApi, IRehldsApi, REGAMEDLL_API_VERSION_MAJOR,
    REGAMEDLL_API_VERSION_MINOR, REHLDS_API_VERSION_MAJOR, REHLDS_API_VERSION_MINOR, ReGameFuncs_t,
    RehldsFuncs_t, VRE_GAMEDLL_API_VERSION, VREHLDS_HLDS_API_VERSION,
};
use crate::capabilities::{ReApiStatus, ReGameCapabilities, RehldsCapabilities};
use goldsrc_api::consts::log_targets;
use std::ffi::{c_char, c_int};
use std::sync::RwLock;

/// Global runtime state of ReAPI detection.
static REAPI_STATE: RwLock<ReApiBridgeState> = RwLock::new(ReApiBridgeState::new());

struct ReApiBridgeState {
    status: ReApiStatus,
    rehlds_api: Option<*const IRehldsApi>,
    rehlds_funcs: Option<*const RehldsFuncs_t>,
    regame_api: Option<*const IReGameApi>,
    regame_funcs: Option<*const ReGameFuncs_t>,
}

unsafe impl Send for ReApiBridgeState {}
unsafe impl Sync for ReApiBridgeState {}

impl ReApiBridgeState {
    const fn new() -> Self {
        Self {
            status: ReApiStatus {
                rehlds_active: false,
                rehlds_major: 0,
                rehlds_minor: 0,
                regamedll_active: false,
                regamedll_major: 0,
                regamedll_minor: 0,
            },
            rehlds_api: None,
            rehlds_funcs: None,
            regame_api: None,
            regame_funcs: None,
        }
    }
}

/// Central orchestrator and safe facade for ReHLDS and ReGameDLL capabilities.
pub struct ReApiBridge;

impl ReApiBridge {
    /// Attempts to initialize ReHLDS from an engine `CreateInterface` factory pointer.
    pub fn try_init_rehlds_factory(factory: CreateInterfaceFn) -> bool {
        unsafe {
            let mut ret_code: c_int = 0;
            let iface_ptr = factory(
                VREHLDS_HLDS_API_VERSION.as_ptr() as *const c_char,
                &mut ret_code,
            );
            if iface_ptr.is_null() {
                log::debug!(target: log_targets::REAPI, "ReHLDS interface not found in engine module");
                return false;
            }

            let rehlds_api = iface_ptr as *const IRehldsApi;
            let vtbl = (*rehlds_api).vtable;
            if vtbl.is_null() {
                log::warn!(target: log_targets::REAPI, "ReHLDS interface found but vtable is null");
                return false;
            }

            let major = ((*vtbl).GetMajorVersion)(iface_ptr);
            let minor = ((*vtbl).GetMinorVersion)(iface_ptr);

            if major != REHLDS_API_VERSION_MAJOR || minor < REHLDS_API_VERSION_MINOR {
                log::warn!(
                    target: log_targets::REAPI,
                    "ReHLDS version mismatch: got {major}.{minor}, expected >={REHLDS_API_VERSION_MAJOR}.{REHLDS_API_VERSION_MINOR}"
                );
                return false;
            }

            let funcs = ((*vtbl).GetFuncs)(iface_ptr);

            if let Ok(mut state) = REAPI_STATE.write() {
                state.status.rehlds_active = true;
                state.status.rehlds_major = major;
                state.status.rehlds_minor = minor;
                state.rehlds_api = Some(rehlds_api);
                state.rehlds_funcs = Some(funcs);
            }

            log::info!(
                target: log_targets::REAPI,
                "ReHLDS API successfully initialized (v{major}.{minor})"
            );
            true
        }
    }

    /// Attempts to initialize ReGameDLL from a GameDLL `CreateInterface` factory pointer.
    pub fn try_init_regamedll_factory(factory: CreateInterfaceFn) -> bool {
        unsafe {
            let mut ret_code: c_int = 0;
            let iface_ptr = factory(
                VRE_GAMEDLL_API_VERSION.as_ptr() as *const c_char,
                &mut ret_code,
            );
            if iface_ptr.is_null() {
                log::debug!(target: log_targets::REAPI, "ReGameDLL interface not found in GameDLL module");
                return false;
            }

            let regame_api = iface_ptr as *const IReGameApi;
            let vtbl = (*regame_api).vtable;
            if vtbl.is_null() {
                log::warn!(target: log_targets::REAPI, "ReGameDLL interface found but vtable is null");
                return false;
            }

            let major = ((*vtbl).GetMajorVersion)(iface_ptr);
            let minor = ((*vtbl).GetMinorVersion)(iface_ptr);

            if major != REGAMEDLL_API_VERSION_MAJOR || minor < REGAMEDLL_API_VERSION_MINOR {
                log::warn!(
                    target: log_targets::REAPI,
                    "ReGameDLL version mismatch: got {major}.{minor}, expected >={REGAMEDLL_API_VERSION_MAJOR}.{REGAMEDLL_API_VERSION_MINOR}"
                );
                return false;
            }

            let funcs = ((*vtbl).GetFuncs)(iface_ptr);

            if let Ok(mut state) = REAPI_STATE.write() {
                state.status.regamedll_active = true;
                state.status.regamedll_major = major;
                state.status.regamedll_minor = minor;
                state.regame_api = Some(regame_api);
                state.regame_funcs = Some(funcs);
            }

            log::info!(
                target: log_targets::REAPI,
                "ReGameDLL API successfully initialized (v{major}.{minor})"
            );
            true
        }
    }

    /// Gets current ReAPI active status snapshot.
    pub fn status() -> ReApiStatus {
        REAPI_STATE.read().map(|s| s.status).unwrap_or(ReApiStatus {
            rehlds_active: false,
            rehlds_major: 0,
            rehlds_minor: 0,
            regamedll_active: false,
            regamedll_major: 0,
            regamedll_minor: 0,
        })
    }

    /// Queries ReHLDS build number.
    pub fn get_build_number() -> Option<i32> {
        let state = REAPI_STATE.read().ok()?;
        let funcs_ptr = state.rehlds_funcs?;
        unsafe {
            let funcs = &*funcs_ptr;
            funcs.GetBuildNumber.map(|f| f())
        }
    }

    /// Queries ReHLDS high-precision real time.
    pub fn get_real_time() -> Option<f64> {
        let state = REAPI_STATE.read().ok()?;
        let funcs_ptr = state.rehlds_funcs?;
        unsafe {
            let funcs = &*funcs_ptr;
            funcs.GetRealTime.map(|f| f())
        }
    }

    /// Automatically scans host process modules for ReHLDS and ReGameDLL if not already loaded.
    pub fn auto_detect() -> ReApiStatus {
        #[cfg(target_os = "windows")]
        {
            // Try detecting ReHLDS from loaded engine module
            if !Self::status().rehlds_active {
                for mod_name in &[c"swds.dll", c"hw.dll"] {
                    unsafe {
                        use windows_sys::Win32::System::LibraryLoader::{
                            GetModuleHandleA, GetProcAddress,
                        };
                        let h_mod = GetModuleHandleA(mod_name.as_ptr().cast());
                        if !h_mod.is_null() {
                            let proc = GetProcAddress(h_mod, c"CreateInterface".as_ptr().cast());
                            if let Some(create_fn) = proc {
                                let factory: CreateInterfaceFn = std::mem::transmute(create_fn);
                                if Self::try_init_rehlds_factory(factory) {
                                    break;
                                }
                            }
                        }
                    }
                }
            }

            // Try detecting ReGameDLL from loaded game DLL module
            if !Self::status().regamedll_active {
                for mod_name in &[c"mp.dll", c"cs.dll"] {
                    unsafe {
                        use windows_sys::Win32::System::LibraryLoader::{
                            GetModuleHandleA, GetProcAddress,
                        };
                        let h_mod = GetModuleHandleA(mod_name.as_ptr().cast());
                        if !h_mod.is_null() {
                            let proc = GetProcAddress(h_mod, c"CreateInterface".as_ptr().cast());
                            if let Some(create_fn) = proc {
                                let factory: CreateInterfaceFn = std::mem::transmute(create_fn);
                                if Self::try_init_regamedll_factory(factory) {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        Self::status()
    }
}

impl RehldsCapabilities for ReApiBridge {
    fn is_rehlds(&self) -> bool {
        Self::status().rehlds_active
    }

    fn get_build_number(&self) -> Option<i32> {
        Self::get_build_number()
    }

    fn get_real_time(&self) -> Option<f64> {
        Self::get_real_time()
    }
}

impl ReGameCapabilities for ReApiBridge {
    fn is_regamedll(&self) -> bool {
        Self::status().regamedll_active
    }
}
