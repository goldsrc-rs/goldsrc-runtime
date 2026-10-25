//! Engine precache operations and registration.

use super::EngineBackend;
use crate::{call_engfunc, call_engfunc_ret};
use goldsrc_spi::engine::EnginePrecache;
use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

static PRECACHE_SOUNDS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));
static PRECACHE_MODELS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));
static PRECACHE_GENERICS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

impl EngineBackend {
    /// Precaches all pending/registered resources during map spawn phase.
    pub fn precache_pending_resources(&self) {
        let default_sounds = [
            "events/tutor_msg.wav",
            "items/suitchargeno1.wav",
            "buttons/bell1.wav",
            "buttons/button1.wav",
            "buttons/blip1.wav",
            "weapons/c4_beep1.wav",
            "common/wpn_denyselect.wav",
        ];
        if let Ok(mut set) = PRECACHE_SOUNDS.lock() {
            for sound in default_sounds {
                set.insert(sound.to_string());
            }
        }

        unsafe {
            if let Ok(sounds) = PRECACHE_SOUNDS.lock() {
                for sound in sounds.iter() {
                    if let Ok(cpath) = std::ffi::CString::new(sound.as_str()) {
                        call_engfunc!((self.engfuncs)().pfnPrecacheSound, cpath.as_ptr());
                    }
                }
            }
            if let Ok(models) = PRECACHE_MODELS.lock() {
                for model in models.iter() {
                    if let Ok(cpath) = std::ffi::CString::new(model.as_str()) {
                        call_engfunc!((self.engfuncs)().pfnPrecacheModel, cpath.as_ptr());
                    }
                }
            }
            if let Ok(generics) = PRECACHE_GENERICS.lock() {
                for generic in generics.iter() {
                    if let Ok(cpath) = std::ffi::CString::new(generic.as_str()) {
                        call_engfunc!((self.engfuncs)().pfnPrecacheGeneric, cpath.as_ptr());
                    }
                }
            }
        }
    }
}

impl EnginePrecache for EngineBackend {
    fn precache_model(&self, path: &str) -> i32 {
        if let Ok(mut set) = PRECACHE_MODELS.lock() {
            set.insert(path.to_string());
        }
        unsafe {
            let cpath = std::ffi::CString::new(path).unwrap_or_default();
            call_engfunc_ret!((self.engfuncs)().pfnPrecacheModel, cpath.as_ptr())
        }
    }

    fn precache_sound(&self, path: &str) -> i32 {
        if let Ok(mut set) = PRECACHE_SOUNDS.lock() {
            set.insert(path.to_string());
        }
        unsafe {
            let cpath = std::ffi::CString::new(path).unwrap_or_default();
            call_engfunc_ret!((self.engfuncs)().pfnPrecacheSound, cpath.as_ptr())
        }
    }

    fn precache_generic(&self, path: &str) -> i32 {
        if let Ok(mut set) = PRECACHE_GENERICS.lock() {
            set.insert(path.to_string());
        }
        unsafe {
            let cpath = std::ffi::CString::new(path).unwrap_or_default();
            call_engfunc_ret!((self.engfuncs)().pfnPrecacheGeneric, cpath.as_ptr())
        }
    }
}
