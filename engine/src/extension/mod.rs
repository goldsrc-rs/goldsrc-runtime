//! Modular Engine Extensions subsystem.
//!
//! Provides a discoverable extension registry and lifecycle coordination.

pub mod registry;

pub use registry::{ExtensionRegistry, extension_registry};
