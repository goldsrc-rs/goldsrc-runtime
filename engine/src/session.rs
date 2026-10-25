//! Unified client session manager, userinfo overrides, and ephemeral player state.

use std::collections::HashMap;

pub use goldsrc_spi::identity::PlayerSessionToken;

/// Ephemeral session state for a connected client slot (1..=32).
#[derive(Debug, Clone)]
pub struct ClientSession {
    /// Monotonically increasing connection generation counter.
    pub generation: u64,
    /// Overridden or cached userinfo key-value pairs (e.g. "_lang", "rate", "name").
    pub userinfo_overrides: HashMap<String, String>,
    /// Custom plugin/system metadata or tags associated with this session.
    pub metadata: HashMap<String, String>,
    /// Slot index of the client (1..=32).
    pub slot: i32,
    /// Engine user ID (`pfnGetPlayerUserId`) assigned by server.
    pub user_id: u32,
}

impl ClientSession {
    /// Creates a new empty session for the given player slot with initial generation.
    pub fn new(slot: i32, generation: u64) -> Self {
        Self {
            slot,
            generation,
            user_id: 0,
            userinfo_overrides: HashMap::new(),
            metadata: HashMap::new(),
        }
    }

    /// Returns the generational session token for this client.
    #[inline(always)]
    pub fn token(&self) -> PlayerSessionToken {
        PlayerSessionToken::new(self.slot, self.generation, self.user_id)
    }

    /// Gets an overridden userinfo value by key (case-insensitive).
    pub fn get_userinfo(&self, key: &str) -> Option<&str> {
        let key_lower = key.to_ascii_lowercase();
        self.userinfo_overrides.get(&key_lower).map(|s| s.as_str())
    }

    /// Sets or overrides a userinfo value by key (case-insensitive).
    pub fn set_userinfo(&mut self, key: &str, value: impl Into<String>) {
        self.userinfo_overrides
            .insert(key.to_ascii_lowercase(), value.into());
    }

    /// Removes a userinfo override by key (case-insensitive).
    pub fn remove_userinfo(&mut self, key: &str) -> Option<String> {
        self.userinfo_overrides.remove(&key.to_ascii_lowercase())
    }

    /// Returns the active language for this session if overridden.
    pub fn lang(&self) -> Option<&str> {
        for key in ["_lang", "lang", "_cl_lang", "cl_lang"] {
            if let Some(val) = self.get_userinfo(key) {
                return Some(val);
            }
        }
        None
    }

    /// Sets the language preference for this session.
    pub fn set_lang(&mut self, lang: &str) {
        self.set_userinfo("_lang", lang);
        self.set_userinfo("lang", lang);
    }
}

/// Thread-safe manager for all connected client sessions (slots 1..=32).
#[derive(Debug, Default)]
pub struct ClientSessionManager {
    sessions: HashMap<i32, ClientSession>,
    generations: HashMap<i32, u64>,
}

impl ClientSessionManager {
    /// Creates an empty session manager.
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            generations: HashMap::new(),
        }
    }

    /// Retrieves a reference to the client session for the given slot.
    pub fn get(&self, slot: i32) -> Option<&ClientSession> {
        self.sessions.get(&slot)
    }

    /// Returns an ordered list of all active client sessions by slot index.
    pub fn active_sessions(&self) -> Vec<&ClientSession> {
        let mut list: Vec<&ClientSession> = self.sessions.values().collect();
        list.sort_by_key(|s| s.slot);
        list
    }

    /// Returns the number of active client sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns `true` if there are no active client sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Verifies if a given generational token is still valid.
    pub fn is_token_valid(&self, token: PlayerSessionToken) -> bool {
        self.sessions
            .get(&token.slot())
            .map(|sess| {
                sess.generation == token.generation()
                    && (token.user_id() == 0
                        || sess.user_id == 0
                        || sess.user_id == token.user_id())
            })
            .unwrap_or(false)
    }

    /// Retrieves a mutable reference to the client session for the given slot, creating it if absent.
    pub fn get_or_create_mut(&mut self, slot: i32) -> &mut ClientSession {
        let generations = &mut self.generations;
        self.sessions.entry(slot).or_insert_with(|| {
            let generation_counter = generations.entry(slot).or_insert(0);
            *generation_counter += 1;
            ClientSession::new(slot, *generation_counter)
        })
    }

    /// Advances the generation counter on connect and initializes a fresh session.
    pub fn on_connect(&mut self, slot: i32, user_id: u32) -> &mut ClientSession {
        let generation_counter = self.generations.entry(slot).or_insert(0);
        *generation_counter += 1;
        let mut session = ClientSession::new(slot, *generation_counter);
        session.user_id = user_id;
        self.sessions.insert(slot, session);
        self.sessions.get_mut(&slot).expect("session just inserted")
    }

    /// Removes session state when a client disconnects, preventing memory leaks.
    pub fn on_disconnect(&mut self, slot: i32) -> Option<ClientSession> {
        self.sessions.remove(&slot)
    }

    /// Clears all active client sessions (e.g. on map change or server shutdown).
    pub fn clear(&mut self) {
        self.sessions.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_session_userinfo_and_lang() {
        let mut mgr = ClientSessionManager::new();
        let session = mgr.get_or_create_mut(1);

        assert_eq!(session.lang(), None);
        session.set_lang("ru");
        assert_eq!(session.lang(), Some("ru"));
        assert_eq!(session.get_userinfo("lang"), Some("ru"));
        assert_eq!(session.get_userinfo("_lang"), Some("ru"));

        session.set_userinfo("rate", "25000");
        assert_eq!(session.get_userinfo("RATE"), Some("25000"));

        mgr.on_disconnect(1);
        assert!(mgr.get(1).is_none());
    }

    #[test]
    fn test_generational_token_slot_recycling() {
        let mut mgr = ClientSessionManager::new();

        // Alice connects to slot 1
        let alice_sess = mgr.on_connect(1, 1001);
        let alice_token = alice_sess.token();
        assert_eq!(alice_token.slot(), 1);
        assert_eq!(alice_token.generation(), 1);
        assert_eq!(alice_token.user_id(), 1001);
        assert!(mgr.is_token_valid(alice_token));

        // Alice disconnects
        mgr.on_disconnect(1);
        assert!(!mgr.is_token_valid(alice_token));

        // Bob connects to slot 1 (recycled slot)
        let bob_sess = mgr.on_connect(1, 1002);
        let bob_token = bob_sess.token();
        assert_eq!(bob_token.slot(), 1);
        assert_eq!(bob_token.generation(), 2);
        assert_eq!(bob_token.user_id(), 1002);

        // Bob's token is valid, but Alice's token is now completely invalid!
        assert!(mgr.is_token_valid(bob_token));
        assert!(!mgr.is_token_valid(alice_token));

        // Mismatched user_id with same slot & generation is also rejected
        let spoofed_bob = PlayerSessionToken::new(1, 2, 9999);
        assert!(!mgr.is_token_valid(spoofed_bob));
    }
}
