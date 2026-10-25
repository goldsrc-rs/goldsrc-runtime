//! Engine physics, trace raycasting, and collision query operations.

use super::EngineBackend;
use crate::{call_engfunc, call_engfunc_ret};
use goldsrc_spi::engine::EnginePhysics;

impl EnginePhysics for EngineBackend {
    fn point_contents(&self, point: [f32; 3]) -> i32 {
        unsafe { call_engfunc_ret!((self.engfuncs)().pfnPointContents, point.as_ptr()) }
    }

    fn trace_line(
        &self,
        start: [f32; 3],
        end: [f32; 3],
        flags: i32,
        ignore_ent: i32,
    ) -> goldsrc_spi::engine::TraceResult {
        unsafe {
            let funcs = (self.engfuncs)();
            let mut raw_trace = std::mem::zeroed::<goldsrc_sys::TraceResult>();
            let pedict = funcs
                .pfnPEntityOfEntIndex
                .map(|f| f(ignore_ent))
                .unwrap_or(std::ptr::null_mut());

            call_engfunc!(
                funcs.pfnTraceLine,
                start.as_ptr(),
                end.as_ptr(),
                flags,
                pedict,
                &mut raw_trace as *mut _
            );

            let hit_id = if raw_trace.pHit.is_null() {
                -1
            } else {
                crate::api_registry::edict_index(raw_trace.pHit)
            };

            goldsrc_spi::engine::TraceResult {
                all_solid: raw_trace.fAllSolid != 0,
                start_solid: raw_trace.fStartSolid != 0,
                in_open: raw_trace.fInOpen != 0,
                in_water: raw_trace.fInWater != 0,
                fraction: raw_trace.flFraction,
                end_pos: raw_trace.vecEndPos,
                plane_normal: raw_trace.vecPlaneNormal,
                hit_entity: hit_id,
            }
        }
    }

    fn trace_hull(
        &self,
        start: [f32; 3],
        end: [f32; 3],
        flags: i32,
        hull_number: i32,
        ignore_ent: i32,
    ) -> goldsrc_spi::engine::TraceResult {
        unsafe {
            let funcs = (self.engfuncs)();
            let mut raw_trace = std::mem::zeroed::<goldsrc_sys::TraceResult>();
            let pedict = funcs
                .pfnPEntityOfEntIndex
                .map(|f| f(ignore_ent))
                .unwrap_or(std::ptr::null_mut());

            call_engfunc!(
                funcs.pfnTraceHull,
                start.as_ptr(),
                end.as_ptr(),
                flags,
                hull_number,
                pedict,
                &mut raw_trace as *mut _
            );

            let hit_id = if raw_trace.pHit.is_null() {
                -1
            } else {
                crate::api_registry::edict_index(raw_trace.pHit)
            };

            goldsrc_spi::engine::TraceResult {
                all_solid: raw_trace.fAllSolid != 0,
                start_solid: raw_trace.fStartSolid != 0,
                in_open: raw_trace.fInOpen != 0,
                in_water: raw_trace.fInWater != 0,
                fraction: raw_trace.flFraction,
                end_pos: raw_trace.vecEndPos,
                plane_normal: raw_trace.vecPlaneNormal,
                hit_entity: hit_id,
            }
        }
    }

    fn trace_model(
        &self,
        start: [f32; 3],
        end: [f32; 3],
        flags: i32,
        ent_index: i32,
    ) -> goldsrc_spi::engine::TraceResult {
        unsafe {
            let funcs = (self.engfuncs)();
            let mut raw_trace = std::mem::zeroed::<goldsrc_sys::TraceResult>();
            let pedict = funcs
                .pfnPEntityOfEntIndex
                .map(|f| f(ent_index))
                .unwrap_or(std::ptr::null_mut());

            if let Some(pfn_trace_model) = funcs.pfnTraceModel {
                pfn_trace_model(
                    start.as_ptr(),
                    end.as_ptr(),
                    flags,
                    pedict,
                    &mut raw_trace as *mut _,
                );

                let hit_id = if raw_trace.pHit.is_null() {
                    -1
                } else {
                    crate::api_registry::edict_index(raw_trace.pHit)
                };

                goldsrc_spi::engine::TraceResult {
                    all_solid: raw_trace.fAllSolid != 0,
                    start_solid: raw_trace.fStartSolid != 0,
                    in_open: raw_trace.fInOpen != 0,
                    in_water: raw_trace.fInWater != 0,
                    fraction: raw_trace.flFraction,
                    end_pos: raw_trace.vecEndPos,
                    plane_normal: raw_trace.vecPlaneNormal,
                    hit_entity: hit_id,
                }
            } else {
                self.trace_line(start, end, flags, ent_index)
            }
        }
    }
}
