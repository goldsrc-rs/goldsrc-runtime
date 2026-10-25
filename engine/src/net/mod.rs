//! Network user message dispatching subsystem.
//!
//! Encapsulates low-level byte-packing protocols for `TextMsg`, `SayText`, HUD messages,
//! and ensures strict adherence to GoldSrc buffer boundaries (185 bytes) and UTF-8 safety.

pub mod dispatcher;

pub use dispatcher::NetworkMessageDispatcher;
