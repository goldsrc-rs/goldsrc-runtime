//! Centralized hook dispatchers, types, and entity VTable hooks.

pub mod dispatcher;
pub mod types;
pub mod vtable;

pub use dispatcher::{dispatch_client_command, dispatch_command, emit};
pub use types::{HookResult, HookTiming};
pub use vtable::{
    GenericVTableLayer, KilledContext, KilledLayer, TakeDamageContext, TakeDamageLayer,
    VTableBridge, VTableTier,
};
