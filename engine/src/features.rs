//! Host-side open-world feature registry.
//!
//! Maintains a zero-allocation flat array of active feature tokens (`[u64; 32]`, 256 bytes).
//! Fits completely in 4 cache lines with zero heap allocations on the hot path.

use goldsrc_spi::hash::FeatureToken;
use std::sync::RwLock;

/// Fixed-capacity flat feature registry with zero heap allocation.
pub struct FeatureRegistry {
    tokens: [u64; 32],
    count: usize,
}

impl Default for FeatureRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureRegistry {
    /// Creates an empty feature registry.
    pub const fn new() -> Self {
        Self {
            tokens: [0; 32],
            count: 0,
        }
    }

    /// Registers a feature token. Returns `true` if registered or already present.
    pub fn register(&mut self, token: FeatureToken) -> bool {
        let raw = token.raw();
        if raw == 0 {
            return false;
        }
        if self.has(token) {
            return true;
        }
        if self.count < self.tokens.len() {
            self.tokens[self.count] = raw;
            self.count += 1;
            true
        } else {
            false
        }
    }

    /// Checks if a feature token is currently registered and active.
    #[inline]
    pub fn has(&self, token: FeatureToken) -> bool {
        let target = token.raw();
        let mut i = 0;
        while i < self.count {
            if self.tokens[i] == target {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Unregisters a feature token.
    pub fn unregister(&mut self, token: FeatureToken) -> bool {
        let target = token.raw();
        for i in 0..self.count {
            if self.tokens[i] == target {
                self.tokens[i] = self.tokens[self.count - 1];
                self.tokens[self.count - 1] = 0;
                self.count -= 1;
                return true;
            }
        }
        false
    }

    /// Clears all registered features.
    pub fn clear(&mut self) {
        self.tokens = [0; 32];
        self.count = 0;
    }
}

static FEATURE_REGISTRY: RwLock<FeatureRegistry> = RwLock::new(FeatureRegistry::new());

/// Registers a feature token globally in the host runtime.
pub fn register_feature(token: FeatureToken) -> bool {
    let mut guard = match FEATURE_REGISTRY.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.register(token)
}

/// Checks if a feature token is active in the host runtime.
pub fn has_feature(token: FeatureToken) -> bool {
    let guard = match FEATURE_REGISTRY.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.has(token)
}

/// Unregisters a feature token globally from the host runtime.
pub fn unregister_feature(token: FeatureToken) -> bool {
    let mut guard = match FEATURE_REGISTRY.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.unregister(token)
}

/// Clears all registered features in the host runtime.
pub fn clear_features() {
    let mut guard = match FEATURE_REGISTRY.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clear();
}

/// Canon feature tokens across the GoldSrc ecosystem.
pub mod canon {
    use goldsrc_spi::hash::FeatureToken;

    /// Counter-Strike 1.6 in-game economy.
    pub const CSTRIKE_ECONOMY: FeatureToken = FeatureToken::from_name("cstrike:economy");

    /// Color chat support (\x03 team-color, \x04 green, \x01 default).
    pub const CSTRIKE_COLOR_CHAT: FeatureToken = FeatureToken::from_name("cstrike:color_chat");

    /// VoiceTranscoder voice protocol upgrade.
    pub const VTC_VOICE: FeatureToken = FeatureToken::from_name("vtc:voice");

    /// ReAPI native engine hooks and member access.
    pub const REAPI_EXT: FeatureToken = FeatureToken::from_name("reapi:ext");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_registry_lifecycle() {
        let mut reg = FeatureRegistry::new();
        assert!(!reg.has(canon::CSTRIKE_ECONOMY));

        assert!(reg.register(canon::CSTRIKE_ECONOMY));
        assert!(reg.has(canon::CSTRIKE_ECONOMY));
        assert!(!reg.has(canon::CSTRIKE_COLOR_CHAT));

        assert!(reg.register(canon::CSTRIKE_COLOR_CHAT));
        assert!(reg.has(canon::CSTRIKE_COLOR_CHAT));

        assert!(reg.unregister(canon::CSTRIKE_ECONOMY));
        assert!(!reg.has(canon::CSTRIKE_ECONOMY));
        assert!(reg.has(canon::CSTRIKE_COLOR_CHAT));

        reg.clear();
        assert!(!reg.has(canon::CSTRIKE_COLOR_CHAT));
    }
}
