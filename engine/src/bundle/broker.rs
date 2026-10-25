//! Inter-bundle message broker for decoupled contract-versioned service requests.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

/// Errors returned by the inter-bundle message broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerError {
    /// The requested service contract is not currently registered or available.
    ServiceUnavailable(String),
    /// The service handler returned an error while processing the request payload.
    HandlerError(String),
    /// The contract identifier is malformed (expected `<namespace>.<version>`, e.g. `economy.v1`).
    InvalidContract(String),
}

impl fmt::Display for BrokerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServiceUnavailable(c) => write!(f, "service contract '{c}' is unavailable"),
            Self::HandlerError(e) => write!(f, "service handler error: {e}"),
            Self::InvalidContract(c) => write!(
                f,
                "invalid contract identifier '{c}'; expected format '<name>.<version>' (e.g. 'economy.v1')"
            ),
        }
    }
}

impl std::error::Error for BrokerError {}

/// Signature for service handlers processing raw byte payloads.
pub type ServiceHandler = Arc<dyn Fn(&[u8]) -> Result<Vec<u8>, String> + Send + Sync>;

/// In-memory message broker managing inter-bundle service registrations and requests.
#[derive(Default)]
pub struct BundleMessageBroker {
    handlers: RwLock<HashMap<String, ServiceHandler>>,
}

impl BundleMessageBroker {
    /// Creates a new, empty bundle message broker.
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(HashMap::new()),
        }
    }

    /// Validates that a contract string adheres to semantic `<service>.<version>` conventions.
    pub fn validate_contract(contract: &str) -> Result<(), BrokerError> {
        let contract = contract.trim();
        if contract.is_empty() {
            return Err(BrokerError::InvalidContract(contract.to_string()));
        }
        let parts: Vec<&str> = contract.split('.').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(BrokerError::InvalidContract(contract.to_string()));
        }
        let is_valid = parts[0]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && parts[1].chars().all(|c| c.is_ascii_alphanumeric());
        if !is_valid {
            return Err(BrokerError::InvalidContract(contract.to_string()));
        }
        Ok(())
    }

    /// Registers a service handler for a contract version.
    pub fn register_service<F>(&self, contract: &str, handler: F) -> Result<(), BrokerError>
    where
        F: Fn(&[u8]) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    {
        Self::validate_contract(contract)?;
        let mut map = self.handlers.write().unwrap();
        map.insert(contract.to_string(), Arc::new(handler));
        Ok(())
    }

    /// Unregisters a previously registered service contract.
    pub fn unregister_service(&self, contract: &str) -> bool {
        let mut map = self.handlers.write().unwrap();
        map.remove(contract).is_some()
    }

    /// Checks if a service contract is currently registered and available.
    pub fn has_service(&self, contract: &str) -> bool {
        let map = self.handlers.read().unwrap();
        map.contains_key(contract)
    }

    /// Dispatches a request to a registered service contract.
    pub fn request(&self, contract: &str, payload: &[u8]) -> Result<Vec<u8>, BrokerError> {
        Self::validate_contract(contract)?;
        let handler = {
            let map = self.handlers.read().unwrap();
            map.get(contract).cloned()
        };

        match handler {
            Some(h) => h(payload).map_err(BrokerError::HandlerError),
            None => Err(BrokerError::ServiceUnavailable(contract.to_string())),
        }
    }

    /// Clears all registered service handlers.
    pub fn clear(&self) {
        let mut map = self.handlers.write().unwrap();
        map.clear();
    }
}

static GLOBAL_BROKER: OnceLock<BundleMessageBroker> = OnceLock::new();

fn global_broker() -> &'static BundleMessageBroker {
    GLOBAL_BROKER.get_or_init(BundleMessageBroker::new)
}

/// Registers a service contract on the global broker.
pub fn register_service<F>(contract: &str, handler: F) -> Result<(), BrokerError>
where
    F: Fn(&[u8]) -> Result<Vec<u8>, String> + Send + Sync + 'static,
{
    global_broker().register_service(contract, handler)
}

/// Dispatches a request to a service contract on the global broker.
pub fn request_service(contract: &str, payload: &[u8]) -> Result<Vec<u8>, BrokerError> {
    global_broker().request(contract, payload)
}

/// Checks whether a service contract is available on the global broker.
pub fn has_service(contract: &str) -> bool {
    global_broker().has_service(contract)
}

/// Unregisters a service contract from the global broker.
pub fn unregister_service(contract: &str) -> bool {
    global_broker().unregister_service(contract)
}

/// Clears all service contracts on the global broker.
pub fn clear_services() {
    global_broker().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_broker_request_response_flow() {
        let broker = BundleMessageBroker::new();
        assert!(!broker.has_service("economy.v1"));

        // Register economy service
        broker
            .register_service("economy.v1", |payload| {
                let text = std::str::from_utf8(payload).map_err(|e| e.to_string())?;
                if text == "get_balance" {
                    Ok(b"1000".to_vec())
                } else {
                    Err("unknown operation".into())
                }
            })
            .unwrap();

        assert!(broker.has_service("economy.v1"));

        // Successful call
        let res = broker.request("economy.v1", b"get_balance").unwrap();
        assert_eq!(res, b"1000");

        // Handler error
        let err = broker.request("economy.v1", b"invalid_op").unwrap_err();
        assert_eq!(
            err,
            BrokerError::HandlerError("unknown operation".to_string())
        );

        // Service unavailable
        let err = broker.request("auth.v1", b"verify").unwrap_err();
        assert_eq!(err, BrokerError::ServiceUnavailable("auth.v1".to_string()));

        // Unregister
        assert!(broker.unregister_service("economy.v1"));
        assert!(!broker.has_service("economy.v1"));
    }

    #[test]
    fn test_contract_validation() {
        assert!(BundleMessageBroker::validate_contract("economy.v1").is_ok());
        assert!(BundleMessageBroker::validate_contract("vip-core.v2").is_ok());
        assert!(BundleMessageBroker::validate_contract("economy").is_err());
        assert!(BundleMessageBroker::validate_contract("economy.v1.extra").is_err());
        assert!(BundleMessageBroker::validate_contract("").is_err());
    }
}
