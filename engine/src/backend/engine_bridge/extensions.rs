//! EngineBackend implementation of EngineExtensions trait.

use crate::backend::EngineBackend;
use crate::extension::extension_registry;
use goldsrc_spi::engine::EngineExtensions;

impl EngineExtensions for EngineBackend {
    fn is_extension_available(&self, name: &str, version_req: Option<&str>) -> bool {
        extension_registry().is_available(name, version_req)
    }

    fn get_extension_version(&self, name: &str) -> Option<String> {
        extension_registry().get_version(name)
    }

    fn list_extensions(&self) -> Vec<(&'static str, String, bool)> {
        extension_registry()
            .all()
            .into_iter()
            .map(|ext| (ext.name(), ext.version().to_string(), ext.is_available()))
            .collect()
    }
}
