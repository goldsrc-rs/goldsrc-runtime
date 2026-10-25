//! Plugin orchestration subsystem.
//!
//! Coordinates multi-layered plugin state synchronization across declarative
//! configuration (`plugins.toml`), reactive rules (`paused_plugins`), and administrator
//! manual overrides (`manual_overrides`).

pub mod orchestrator;

pub use orchestrator::PluginOrchestrator;
