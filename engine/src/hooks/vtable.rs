//! C++ Virtual Table (VTable) and Entity Lifecycle Hooks based on `stitch-rs`.
//!
//! Provides two-tier dynamic virtual function interception for `CBaseEntity`, `CBasePlayer`,
//! and weapon/item entities (e.g. `TakeDamage`, `Killed`, `TraceAttack`, `Spawn`, `ResetMaxSpeed`):
//! - Tier 1: ReGameDLL API hooks (direct API hookchains when ReGameDLL is active).
//! - Tier 2: Dynamic C++ VTable virtual function table hooking fallback.
//!
//! Evaluates hooks through a monomorphic U-Cycle pipeline implementing The Sewing Machine Architecture (SMA):
//! - Descent (`on_enter`): Filter & Protection checks with immediate short-circuit/halt (`FlowControl::Halt`).
//! - Puncture point (`TerminalHandler`): Mutation and algebraic calculations.
//! - Ascent (`on_exit`): Observations, metrics, sound, visual feedback, and post-strike telemetry.

use crate::hooks::types::{HookResult, HookTiming};
use goldsrc_api::gamedata::VTableFunc;
use goldsrc_api::modifiers::{CommutativeModifier, TypedBlackboard};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};
use stitch_rs::flow::FlowControl;

/// Operational tier active for entity VTable interception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VTableTier {
    /// Tier 1: Native ReGameDLL extended hookchain API.
    Tier1ReGame,
    /// Tier 2: C++ VTable virtual function table hooking fallback.
    Tier2VTable,
}

/// Payload for `TakeDamage` virtual function call.
#[repr(C, align(64))]
#[derive(Debug, Clone, PartialEq)]
pub struct TakeDamageContext {
    /// Algebraic, order-independent damage modifier pipeline.
    pub modifiers: CommutativeModifier,
    /// Context blackboard for inter-plugin auxiliary metadata.
    pub blackboard: TypedBlackboard,
    /// Victim entity index.
    pub victim: i32,
    /// Inflictor entity index (e.g. grenade, rocket, or weapon holder).
    pub inflictor: i32,
    /// Attacker entity index (e.g. player who fired).
    pub attacker: i32,
    /// Amount of damage being dealt (can be mutated in Pre-hook or resolved via `modifiers`).
    pub damage: f32,
    /// Damage type bits (`DMG_GENERIC`, `DMG_BULLET`, `DMG_BLAST`, etc.).
    pub bits_damage_type: i32,
}

impl TakeDamageContext {
    /// Creates a new `TakeDamageContext` initializing the commutative modifier pipeline with base damage.
    pub fn new(
        victim: i32,
        inflictor: i32,
        attacker: i32,
        damage: f32,
        bits_damage_type: i32,
    ) -> Self {
        Self {
            victim,
            inflictor,
            attacker,
            damage,
            bits_damage_type,
            modifiers: CommutativeModifier::new(damage),
            blackboard: TypedBlackboard::new(),
        }
    }

    /// Synchronizes `self.damage` with the computed commutative modifier result.
    pub fn sync_damage(&mut self) -> f32 {
        if !self.modifiers.flat_bonuses.is_empty()
            || !self.modifiers.multipliers.is_empty()
            || !self.modifiers.reductions.is_empty()
            || self.modifiers.is_blocked
        {
            self.damage = self.modifiers.compute();
        } else {
            self.modifiers.base = self.damage;
        }
        self.damage
    }
}

impl stitch_rs::Blackboard for TakeDamageContext {}

/// Payload for `Killed` virtual function call.
#[repr(C, align(64))]
#[derive(Debug, Clone, PartialEq)]
pub struct KilledContext {
    /// Victim entity index.
    pub victim: i32,
    /// Attacker entity index.
    pub attacker: i32,
    /// Gib behavior mode (`GIB_NORMAL`, `GIB_NEVER`, `GIB_ALWAYS`).
    pub gib_mode: i32,
}

impl stitch_rs::Blackboard for KilledContext {}

/// Abstract take damage interceptor middleware based on `stitch-rs`.
pub trait TakeDamageLayer: Send + Sync {
    /// Descent phase: inspection, mutation, or short-circuiting.
    fn on_enter(&self, ctx: &mut TakeDamageContext) -> FlowControl<(), (), HookResult<i32>>;
    /// Ascent phase: observation, post-damage telemetry, or metrics.
    fn on_exit(&self, ctx: &mut TakeDamageContext, outcome: &mut Result<(), HookResult<i32>>);
}

/// Abstract killed interceptor middleware based on `stitch-rs`.
pub trait KilledLayer: Send + Sync {
    /// Descent phase: filter or short-circuit.
    fn on_enter(&self, ctx: &KilledContext) -> FlowControl<(), (), HookResult<()>>;
    /// Ascent phase: audit logging, stats recording.
    fn on_exit(&self, ctx: &KilledContext, outcome: &mut Result<(), HookResult<()>>);
}

/// Abstract generic VTable interceptor middleware for entity lifecycle events (`Spawn`, `Touch`, `Use`, `ResetMaxSpeed`).
pub trait GenericVTableLayer: Send + Sync {
    /// Descent phase: inspection or vetoing.
    fn on_enter(&self, entity_idx: i32, timing: HookTiming) -> FlowControl<(), (), HookResult<()>>;
    /// Ascent phase: observation and post-action notification.
    fn on_exit(
        &self,
        entity_idx: i32,
        timing: HookTiming,
        outcome: &mut Result<(), HookResult<()>>,
    );
}

/// Dynamic SMA Pipeline Chain for VTable Events.
#[derive(Default)]
struct VTablePipelineRegistry {
    damage_layers: Vec<Arc<dyn TakeDamageLayer>>,
    killed_layers: Vec<Arc<dyn KilledLayer>>,
    generic_layers: HashMap<VTableFunc, Vec<Arc<dyn GenericVTableLayer>>>,
}

static VTABLE_REGISTRY: LazyLock<RwLock<VTablePipelineRegistry>> =
    LazyLock::new(|| RwLock::new(VTablePipelineRegistry::default()));

/// Central facade for entity C++ VTable interception and lifecycle dispatching.
pub struct VTableBridge;

impl VTableBridge {
    /// Resolves the currently active combat/VTable interception tier.
    pub fn active_tier() -> VTableTier {
        if crate::extension::extension_registry().is_available("regamedll", None) {
            VTableTier::Tier1ReGame
        } else {
            VTableTier::Tier2VTable
        }
    }

    /// Registers an authentic SMA `TakeDamageLayer`.
    pub fn register_take_damage_layer(layer: Arc<dyn TakeDamageLayer>) {
        if let Ok(mut reg) = VTABLE_REGISTRY.write() {
            reg.damage_layers.push(layer);
        }
    }

    /// Registers an authentic SMA `KilledLayer`.
    pub fn register_killed_layer(layer: Arc<dyn KilledLayer>) {
        if let Ok(mut reg) = VTABLE_REGISTRY.write() {
            reg.killed_layers.push(layer);
        }
    }

    /// Registers an authentic SMA `GenericVTableLayer` for a specific virtual method.
    pub fn register_generic_layer(func: VTableFunc, layer: Arc<dyn GenericVTableLayer>) {
        if let Ok(mut reg) = VTABLE_REGISTRY.write() {
            reg.generic_layers.entry(func).or_default().push(layer);
        }
    }

    /// Dispatches a `TakeDamage` event through the complete SMA pipeline.
    /// Returns the final hook verdict and the calculated mutated damage.
    pub fn dispatch_take_damage(
        victim: i32,
        inflictor: i32,
        attacker: i32,
        damage: f32,
        bits_damage_type: i32,
    ) -> (HookResult<i32>, f32) {
        let mut ctx = TakeDamageContext::new(victim, inflictor, attacker, damage, bits_damage_type);

        let mut halted_res = None;
        let mut entered_count = 0;

        let outcome = {
            let reg = match VTABLE_REGISTRY.read() {
                Ok(r) => r,
                Err(e) => e.into_inner(),
            };
            let layers = &reg.damage_layers;

            for layer in layers {
                match layer.on_enter(&mut ctx) {
                    FlowControl::Proceed(()) => {
                        ctx.sync_damage();
                        entered_count += 1;
                    }
                    FlowControl::ShortCircuit(()) => {
                        ctx.sync_damage();
                        entered_count += 1;
                        break;
                    }
                    FlowControl::Halt(res) => {
                        halted_res = Some(res);
                        break;
                    }
                }
            }

            let mut out = match halted_res {
                Some(res) => Err(res),
                None => Ok(()),
            };

            for layer in layers.iter().take(entered_count).rev() {
                layer.on_exit(&mut ctx, &mut out);
            }
            out
        };

        crate::hooks::dispatcher::emit(crate::host::HostEvent::EntityTakeDamage {
            victim: ctx.victim,
            inflictor: ctx.inflictor,
            attacker: ctx.attacker,
            damage: ctx.damage,
            bits_damage_type: ctx.bits_damage_type,
            timing: HookTiming::Pre,
        });

        if let Err(res) = outcome {
            return (res, 0.0);
        }

        let final_damage = ctx.sync_damage();
        (HookResult::Handled, final_damage)
    }

    /// Dispatches a `Killed` event through the complete SMA pipeline.
    pub fn dispatch_killed(victim: i32, attacker: i32, gib_mode: i32) -> HookResult<()> {
        let ctx = KilledContext {
            victim,
            attacker,
            gib_mode,
        };

        let mut halted_res = None;
        let mut entered_count = 0;

        let outcome = {
            let reg = match VTABLE_REGISTRY.read() {
                Ok(r) => r,
                Err(e) => e.into_inner(),
            };
            let layers = &reg.killed_layers;

            for layer in layers {
                match layer.on_enter(&ctx) {
                    FlowControl::Proceed(()) => {
                        entered_count += 1;
                    }
                    FlowControl::ShortCircuit(()) => {
                        entered_count += 1;
                        break;
                    }
                    FlowControl::Halt(res) => {
                        halted_res = Some(res);
                        break;
                    }
                }
            }

            let mut out = match halted_res {
                Some(res) => Err(res),
                None => Ok(()),
            };

            for layer in layers.iter().take(entered_count).rev() {
                layer.on_exit(&ctx, &mut out);
            }
            out
        };

        crate::hooks::dispatcher::emit(crate::host::HostEvent::EntityKilled {
            victim: ctx.victim,
            attacker: ctx.attacker,
            gib_mode: ctx.gib_mode,
            timing: HookTiming::Pre,
        });

        if let Err(res) = outcome {
            return res;
        }

        HookResult::Handled
    }

    /// Dispatches a generic VTable event (`Spawn`, `Touch`, `Use`, `ResetMaxSpeed`).
    pub fn dispatch_generic(
        func: VTableFunc,
        entity_idx: i32,
        timing: HookTiming,
    ) -> HookResult<()> {
        let mut halted_res = None;
        let mut entered_count = 0;

        let outcome = {
            let reg = match VTABLE_REGISTRY.read() {
                Ok(r) => r,
                Err(e) => e.into_inner(),
            };

            if let Some(layers) = reg.generic_layers.get(&func) {
                for layer in layers {
                    match layer.on_enter(entity_idx, timing) {
                        FlowControl::Proceed(()) => {
                            entered_count += 1;
                        }
                        FlowControl::ShortCircuit(()) => {
                            entered_count += 1;
                            break;
                        }
                        FlowControl::Halt(res) => {
                            halted_res = Some(res);
                            break;
                        }
                    }
                }

                let mut out = match halted_res {
                    Some(res) => Err(res),
                    None => Ok(()),
                };

                for layer in layers.iter().take(entered_count).rev() {
                    layer.on_exit(entity_idx, timing, &mut out);
                }
                out
            } else {
                Ok(())
            }
        };

        if let Err(res) = outcome {
            return res;
        }

        HookResult::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ProtectionFilterLayer;
    impl TakeDamageLayer for ProtectionFilterLayer {
        fn on_enter(&self, ctx: &mut TakeDamageContext) -> FlowControl<(), (), HookResult<i32>> {
            if ctx.victim == 99 {
                FlowControl::Halt(HookResult::Supercede(0))
            } else {
                FlowControl::Proceed(())
            }
        }
        fn on_exit(
            &self,
            _ctx: &mut TakeDamageContext,
            _outcome: &mut Result<(), HookResult<i32>>,
        ) {
        }
    }

    struct PerkMultiplierLayer;
    impl TakeDamageLayer for PerkMultiplierLayer {
        fn on_enter(&self, ctx: &mut TakeDamageContext) -> FlowControl<(), (), HookResult<i32>> {
            ctx.modifiers.add_multiplier("perk_2x", 2.0);
            FlowControl::Proceed(())
        }
        fn on_exit(
            &self,
            _ctx: &mut TakeDamageContext,
            _outcome: &mut Result<(), HookResult<i32>>,
        ) {
        }
    }

    struct BlackboardLayer;
    impl TakeDamageLayer for BlackboardLayer {
        fn on_enter(&self, ctx: &mut TakeDamageContext) -> FlowControl<(), (), HookResult<i32>> {
            ctx.blackboard.set_bool("is_critical", true);
            FlowControl::Proceed(())
        }
        fn on_exit(
            &self,
            _ctx: &mut TakeDamageContext,
            _outcome: &mut Result<(), HookResult<i32>>,
        ) {
        }
    }

    #[test]
    fn test_vtable_bridge_phased_damage_flow() {
        VTableBridge::register_take_damage_layer(Arc::new(ProtectionFilterLayer));
        VTableBridge::register_take_damage_layer(Arc::new(PerkMultiplierLayer));
        VTableBridge::register_take_damage_layer(Arc::new(BlackboardLayer));

        // Test normal victim with 2x multiplier
        let (res, dmg) = VTableBridge::dispatch_take_damage(1, 2, 2, 50.0, 0);
        assert!(!res.is_superceded());
        assert!((dmg - 100.0).abs() < 1e-4);

        // Test protected victim 99
        let (res, dmg) = VTableBridge::dispatch_take_damage(99, 2, 2, 50.0, 0);
        assert!(res.is_superceded());
        assert_eq!(dmg, 0.0);
    }

    struct GenericTestLayer {
        called: Arc<std::sync::atomic::AtomicBool>,
    }
    impl GenericVTableLayer for GenericTestLayer {
        fn on_enter(&self, idx: i32, _timing: HookTiming) -> FlowControl<(), (), HookResult<()>> {
            if idx == 42 {
                self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            FlowControl::Proceed(())
        }
        fn on_exit(
            &self,
            _idx: i32,
            _timing: HookTiming,
            _outcome: &mut Result<(), HookResult<()>>,
        ) {
        }
    }

    #[test]
    fn test_generic_vtable_dispatch() {
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        VTableBridge::register_generic_layer(
            VTableFunc::ResetMaxSpeed,
            Arc::new(GenericTestLayer {
                called: called.clone(),
            }),
        );

        let res = VTableBridge::dispatch_generic(VTableFunc::ResetMaxSpeed, 42, HookTiming::Post);
        assert_eq!(res, HookResult::Handled);
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
