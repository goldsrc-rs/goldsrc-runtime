//! Modular Metamod plugin manager engine extension.
//!
//! Provides the `MetamodExtension` descriptor and runtime capabilities
//! for plugins to discover and interact with the Metamod plugin manager.

pub mod api;
pub mod types;

pub use api::{MetamodApi, set_meta_globals, set_meta_util_funcs};
pub use types::*;

use goldsrc_spi::ExtensionPort;
use std::sync::Arc;

/// Metamod plugin manager engine extension (`ext:metamod`).
pub struct MetamodExtension {
    version: &'static str,
    active: bool,
}

impl MetamodExtension {
    /// Creates a new Metamod extension descriptor.
    pub const fn new(active: bool) -> Self {
        Self {
            active,
            version: "1.21.0",
        }
    }

    /// Sets the discovered Metamod version.
    pub fn with_version(active: bool, version: &'static str) -> Self {
        Self { active, version }
    }
}

impl ExtensionPort for MetamodExtension {
    fn name(&self) -> &'static str {
        "metamod"
    }

    fn version(&self) -> &str {
        self.version
    }

    fn is_available(&self) -> bool {
        self.active
    }

    fn description(&self) -> &str {
        "Metamod Plugin Manager Engine Extension"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Initializes and registers the Metamod engine extension into the global registry.
pub fn init(active: bool) {
    let registry = goldsrc_core::extension::extension_registry();
    registry.register(Arc::new(MetamodExtension::new(active)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metamod_extension() {
        let ext = MetamodExtension::new(true);
        assert_eq!(ext.name(), "metamod");
        assert!(ext.is_available());
        assert_eq!(ext.version(), "1.21.0");
    }

    #[test]
    fn test_metamod_api_defaults() {
        assert!(!MetamodApi::is_available());
        assert_eq!(MetamodApi::get_result(), MRES_UNSET);
        assert_eq!(MetamodApi::get_user_msg_id("TestMsg"), None);
        assert_eq!(MetamodApi::get_user_msg_name(1), None);
    }
}
