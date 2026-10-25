use crate::rules::{ServerRuleContext, create_default_server_rule_registry};
use goldsrc_api::rules::{Rule, RuleEngine, RuleScope};
use std::collections::{HashMap, HashSet};

/// Orchestrator for declarative reactive rules, scoped lifecycle evaluation,
/// and administrator manual override tracking.
pub struct RuleOrchestrator {
    /// Active rules compiled from configuration.
    rules: Vec<Rule>,
    /// Deliberate manual overrides initiated by administrator console commands (e.g. `grs pause`, `grs unpause`).
    /// Key: plugin name, Value: is_paused.
    /// Manual overrides strictly take precedence over reactive rule evaluations.
    manual_overrides: HashMap<String, bool>,
    /// Tracked set of rules that were satisfied on the last evaluation run.
    active_rules: HashSet<String>,
}

impl Default for RuleOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleOrchestrator {
    /// Creates a new empty [`RuleOrchestrator`].
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            manual_overrides: HashMap::new(),
            active_rules: HashSet::new(),
        }
    }

    /// Sets or updates the active rule set.
    pub fn set_rules(&mut self, rules: Vec<Rule>) {
        self.rules = rules;
    }

    /// Returns a reference to the active rules.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Returns a reference to the active manual overrides.
    pub fn manual_overrides(&self) -> &HashMap<String, bool> {
        &self.manual_overrides
    }

    /// Records a manual pause state override for a plugin (e.g. from admin CLI `grs pause` / `grs unpause`).
    pub fn set_manual_override(&mut self, plugin_name: &str, is_paused: bool) {
        self.manual_overrides
            .insert(plugin_name.to_string(), is_paused);
    }

    /// Removes a manual pause state override for a plugin.
    pub fn remove_manual_override(&mut self, plugin_name: &str) -> Option<bool> {
        self.manual_overrides.remove(plugin_name)
    }

    /// Clears all manual overrides.
    pub fn clear_manual_overrides(&mut self) {
        self.manual_overrides.clear();
    }

    /// Resets transient state and manual overrides on map change.
    pub fn on_map_change(&mut self) {
        self.active_rules.clear();
        self.manual_overrides.clear();
    }

    /// Evaluates rules matching `scope` against the server context.
    /// Returns the execution results for satisfied rules.
    pub fn evaluate_scope(
        &mut self,
        scope: &RuleScope,
        ctx: &mut ServerRuleContext,
    ) -> Vec<(String, Result<(), Vec<String>>)> {
        let registry = create_default_server_rule_registry();
        let engine = RuleEngine::new(registry, self.rules.clone());
        let results = engine.evaluate_and_execute_scope(ctx, scope);

        // Update active rules tracking
        for (rule_name, res) in &results {
            if res.is_ok() {
                self.active_rules.insert(rule_name.clone());
            }
        }

        results
    }
}
