//! Dynamic Registry for modular Engine Extensions.
//!
//! Provides thread-safe extension discovery, version requirement checking
//! via `semver`, and lifecycle event broadcasting.

use goldsrc_api::consts::log_targets;
use goldsrc_spi::ExtensionPort;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

/// Thread-safe registry holding all discoverable engine extensions.
#[derive(Default)]
pub struct ExtensionRegistry {
    extensions: RwLock<HashMap<String, Arc<dyn ExtensionPort>>>,
}

impl ExtensionRegistry {
    /// Creates a new empty extension registry.
    pub fn new() -> Self {
        Self {
            extensions: RwLock::new(HashMap::new()),
        }
    }

    /// Registers an extension into the registry.
    pub fn register(&self, extension: Arc<dyn ExtensionPort>) {
        let name = extension.name().to_ascii_lowercase();
        log::debug!(
            target: log_targets::CORE,
            "Registering engine extension '{name}' (v{}, active: {})",
            extension.version(),
            extension.is_available()
        );
        if let Ok(mut map) = self.extensions.write() {
            map.insert(name, extension);
        }
    }

    /// Retrieves an extension by its canonical name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn ExtensionPort>> {
        let name_lower = name.to_ascii_lowercase();
        self.extensions
            .read()
            .ok()
            .and_then(|map| map.get(&name_lower).cloned())
    }

    /// Returns `true` if the specified extension is registered, active, and satisfies
    /// the optional semantic version constraint.
    pub fn is_available(&self, name: &str, version_req: Option<&str>) -> bool {
        let ext = match self.get(name) {
            Some(e) => e,
            None => return false,
        };

        if !ext.is_available() {
            return false;
        }

        let Some(req_str) = version_req else {
            return true;
        };

        Self::check_version(ext.version(), req_str)
    }

    /// Checks if a version string satisfies a requirement string using SemVer.
    fn check_version(version_str: &str, req_str: &str) -> bool {
        // Try strict or lenient SemVer parse
        let parsed_ver = semver::Version::parse(version_str)
            .or_else(|_| {
                let parts: Vec<&str> = version_str.split('.').collect();
                if parts.len() == 2 {
                    semver::Version::parse(&format!("{version_str}.0"))
                } else if parts.len() == 1 {
                    semver::Version::parse(&format!("{version_str}.0.0"))
                } else {
                    semver::Version::parse("0.0.0")
                }
            })
            .ok();

        let parsed_req = semver::VersionReq::parse(req_str).ok();

        match (parsed_ver, parsed_req) {
            (Some(ver), Some(req)) => req.matches(&ver),
            // Fallback: simple exact string prefix or equality match
            _ => version_str == req_str || version_str.starts_with(req_str),
        }
    }

    /// Gets the current version string of an extension if registered.
    pub fn get_version(&self, name: &str) -> Option<String> {
        self.get(name).map(|e| e.version().to_string())
    }

    /// Returns a list of all registered extensions.
    pub fn all(&self) -> Vec<Arc<dyn ExtensionPort>> {
        self.extensions
            .read()
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Broadcasts the server frame tick to all active extensions.
    pub fn on_server_frame(&self) {
        if let Ok(map) = self.extensions.read() {
            for ext in map.values() {
                if ext.is_available() {
                    ext.on_server_frame();
                }
            }
        }
    }

    /// Broadcasts level change to all active extensions.
    pub fn on_change_level(&self, map_name: &str) {
        if let Ok(map) = self.extensions.read() {
            for ext in map.values() {
                if ext.is_available() {
                    ext.on_change_level(map_name);
                }
            }
        }
    }

    /// Broadcasts shutdown to all registered extensions.
    pub fn on_shutdown(&self) {
        if let Ok(map) = self.extensions.read() {
            for ext in map.values() {
                ext.on_shutdown();
            }
        }
    }
}

/// Global extension registry singleton.
pub fn extension_registry() -> &'static ExtensionRegistry {
    static REGISTRY: OnceLock<ExtensionRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ExtensionRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockExtension {
        name: &'static str,
        version: &'static str,
        active: bool,
    }

    impl ExtensionPort for MockExtension {
        fn name(&self) -> &'static str {
            self.name
        }
        fn version(&self) -> &str {
            self.version
        }
        fn is_available(&self) -> bool {
            self.active
        }
        fn description(&self) -> &str {
            "Mock test extension"
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn test_extension_registry_registration_and_semver() {
        let registry = ExtensionRegistry::new();
        registry.register(Arc::new(MockExtension {
            name: "reapi",
            version: "5.26.0",
            active: true,
        }));
        registry.register(Arc::new(MockExtension {
            name: "disabled_ext",
            version: "1.0.0",
            active: false,
        }));

        assert!(registry.is_available("reapi", None));
        assert!(registry.is_available("reapi", Some(">=5.21.0")));
        assert!(registry.is_available("reapi", Some("^5.0")));
        assert!(!registry.is_available("reapi", Some(">=6.0.0")));

        // Disabled extension should return false even if semver matches
        assert!(!registry.is_available("disabled_ext", None));
        assert!(!registry.is_available("disabled_ext", Some(">=1.0.0")));

        // Missing extension
        assert!(!registry.is_available("nonexistent", None));
    }
}
