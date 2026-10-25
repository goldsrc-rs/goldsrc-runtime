//! Engine 2D/3D sound emission operations.

use super::EngineBackend;
use crate::call_engfunc;
use goldsrc_spi::engine::EngineSound;

impl EngineSound for EngineBackend {
    fn emit_sound(
        &self,
        entity: i32,
        channel: i32,
        sample: &str,
        volume: f32,
        attenuation: f32,
        flags: i32,
        pitch: i32,
    ) {
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = funcs
                .pfnPEntityOfEntIndex
                .map(|f| f(entity))
                .unwrap_or(std::ptr::null_mut());
            let cstr = std::ffi::CString::new(sample).unwrap_or_default();
            call_engfunc!(
                funcs.pfnEmitSound,
                pedict,
                channel,
                cstr.as_ptr(),
                volume,
                attenuation,
                flags,
                pitch
            );
        }
    }

    fn emit_ambient_sound(
        &self,
        entity: i32,
        pos: [f32; 3],
        sample: &str,
        volume: f32,
        attenuation: f32,
        flags: i32,
        pitch: i32,
    ) {
        unsafe {
            let funcs = (self.engfuncs)();
            let pedict = funcs
                .pfnPEntityOfEntIndex
                .map(|f| f(entity))
                .unwrap_or(std::ptr::null_mut());
            let mut pos_copy = pos;
            let cstr = std::ffi::CString::new(sample).unwrap_or_default();
            call_engfunc!(
                funcs.pfnEmitAmbientSound,
                pedict,
                pos_copy.as_mut_ptr(),
                cstr.as_ptr(),
                volume,
                attenuation,
                flags,
                pitch
            );
        }
    }
}
