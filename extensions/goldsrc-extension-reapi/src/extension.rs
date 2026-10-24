//! ReAPI subsystem modular engine extensions.
//!
//! Exposes ReHLDS and ReGameDLL capabilities as first-class `ExtensionPort` instances
//! queryable by plugins via `ext:reapi`, `ext:rehlds`, and `ext:regamedll`.

use crate::bridge::ReApiBridge;
use crate::capabilities::{ReGameCapabilities, RehldsCapabilities};
use goldsrc_spi::ExtensionPort;
use std::any::Any;

/// Composite ReAPI engine extension (`ext:reapi`).
pub struct ReApiExtension;

impl ExtensionPort for ReApiExtension {
    fn name(&self) -> &'static str {
        "reapi"
    }

    fn version(&self) -> &str {
        let status = ReApiBridge::status();
        if status.regamedll_active {
            "5.26.0"
        } else if status.rehlds_active {
            "3.14.0"
        } else {
            "0.0.0"
        }
    }

    fn is_available(&self) -> bool {
        ReApiBridge::status().is_available()
    }

    fn description(&self) -> &str {
        "ReHLDS Engine & ReGameDLL Game Rules Composite Extension"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl RehldsCapabilities for ReApiExtension {
    fn is_rehlds(&self) -> bool {
        ReApiBridge::status().rehlds_active
    }

    fn get_build_number(&self) -> Option<i32> {
        ReApiBridge::get_build_number()
    }

    fn get_real_time(&self) -> Option<f64> {
        ReApiBridge::get_real_time()
    }
}

impl ReGameCapabilities for ReApiExtension {
    fn is_regamedll(&self) -> bool {
        ReApiBridge::status().regamedll_active
    }
}

/// ReHLDS dedicated server engine extension (`ext:rehlds`).
pub struct RehldsExtension;

impl ExtensionPort for RehldsExtension {
    fn name(&self) -> &'static str {
        "rehlds"
    }

    fn version(&self) -> &str {
        let status = ReApiBridge::status();
        if status.rehlds_active {
            "3.14.0"
        } else {
            "0.0.0"
        }
    }

    fn is_available(&self) -> bool {
        ReApiBridge::status().rehlds_active
    }

    fn description(&self) -> &str {
        "ReHLDS High-Performance Engine Extension"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl RehldsCapabilities for RehldsExtension {
    fn is_rehlds(&self) -> bool {
        ReApiBridge::status().rehlds_active
    }

    fn get_build_number(&self) -> Option<i32> {
        ReApiBridge::get_build_number()
    }

    fn get_real_time(&self) -> Option<f64> {
        ReApiBridge::get_real_time()
    }
}

/// ReGameDLL game logic extension (`ext:regamedll`).
pub struct ReGameDllExtension;

impl ExtensionPort for ReGameDllExtension {
    fn name(&self) -> &'static str {
        "regamedll"
    }

    fn version(&self) -> &str {
        let status = ReApiBridge::status();
        if status.regamedll_active {
            "5.26.0"
        } else {
            "0.0.0"
        }
    }

    fn is_available(&self) -> bool {
        ReApiBridge::status().regamedll_active
    }

    fn description(&self) -> &str {
        "ReGameDLL Counter-Strike 1.6 Game Logic Extension"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl ReGameCapabilities for ReGameDllExtension {
    fn is_regamedll(&self) -> bool {
        ReApiBridge::status().regamedll_active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_names_and_status() {
        let reapi = ReApiExtension;
        assert_eq!(reapi.name(), "reapi");
        assert_eq!(reapi.version(), "0.0.0");
        assert!(!reapi.is_available());

        let rehlds = RehldsExtension;
        assert_eq!(rehlds.name(), "rehlds");
        assert_eq!(rehlds.version(), "0.0.0");
        assert!(!rehlds.is_available());

        let regamedll = ReGameDllExtension;
        assert_eq!(regamedll.name(), "regamedll");
        assert_eq!(regamedll.version(), "0.0.0");
        assert!(!regamedll.is_available());
    }
}
