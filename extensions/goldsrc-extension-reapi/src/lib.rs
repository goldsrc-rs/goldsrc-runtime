//! Modular ReAPI Engine Extension for GoldSrc.rs.
//!
//! Provides dynamic detection and capabilities for ReHLDS and ReGameDLL.
//! When registered into `ExtensionRegistry`, plugins can declare
//! requirements such as `ext:reapi`, `ext:rehlds`, or `ext:regamedll`.

pub mod bridge;
pub mod c_abi;
pub mod capabilities;
pub mod extension;

pub use bridge::ReApiBridge;
pub use capabilities::{ReApiStatus, ReGameCapabilities, RehldsCapabilities};
pub use extension::{ReApiExtension, ReGameDllExtension, RehldsExtension};

use goldsrc_core::extension::extension_registry;
use std::sync::Arc;

/// Registers all ReAPI extensions (composite reapi, rehlds, regamedll) into
/// the central engine `ExtensionRegistry`.
pub fn register_extensions() {
    let registry = extension_registry();
    registry.register(Arc::new(ReApiExtension));
    registry.register(Arc::new(RehldsExtension));
    registry.register(Arc::new(ReGameDllExtension));
}

/// Automatically detects ReHLDS and ReGameDLL if present in the process,
/// and registers the extensions in the global registry.
pub fn init() {
    ReApiBridge::auto_detect();
    register_extensions();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_extensions() {
        register_extensions();
        let reg = extension_registry();
        let exts = reg.all();
        assert!(exts.iter().any(|e| e.name() == "reapi"));
        assert!(exts.iter().any(|e| e.name() == "rehlds"));
        assert!(exts.iter().any(|e| e.name() == "regamedll"));
    }
}
