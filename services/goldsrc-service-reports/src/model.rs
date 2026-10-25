//! Domain models, event records, and summary schemas for reports.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for a report.
pub type ReportId = String;

/// Conceptual classification of report workloads.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReportKind {
    /// Full competitive match or scrim session.
    MatchSession,
    /// Per-round granular combat and economy telemetry.
    RoundTelemetry,
    /// Player connection, duration, and session metrics.
    PlayerActivity,
    /// Host runtime and engine performance health snapshots.
    ServerHealth,
    /// Custom plugin-defined report type.
    Custom(String),
}

/// Lifecycle state machine of an aggregated report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportStatus {
    /// Actively receiving events.
    Active,
    /// Finalized, summary calculated, ready for export.
    Finalized,
    /// Dispatched through all registered exporters.
    Flushed,
}

/// Discrete structured telemetry event recorded inside a report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ReportEvent {
    /// Damage dealt from one entity/player to another.
    Damage {
        attacker: i32,
        victim: i32,
        damage: f32,
        weapon: String,
        hitgroup: i32,
        timestamp_ms: u64,
    },
    /// Kill / elimination event.
    Kill {
        attacker: i32,
        victim: i32,
        weapon: String,
        headshot: bool,
        timestamp_ms: u64,
    },
    /// Round outcome and scoring.
    RoundOutcome {
        round_num: u32,
        winner_team: String,
        duration_secs: f32,
        reason: String,
    },
    /// Player connect event.
    Connect {
        player: i32,
        auth: String,
        ip: String,
        timestamp_ms: u64,
    },
    /// Player disconnect and playtime tracking.
    Disconnect {
        player: i32,
        auth: String,
        duration_connected_secs: u64,
        reason: String,
    },
    /// Arbitrary structured plugin event.
    Custom {
        tag: String,
        data: serde_json::Value,
    },
}

/// Aggregated statistical summary calculated upon report finalization.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportSummary {
    /// Score per team (e.g. {"TERRORIST": 16, "CT": 14}).
    pub team_scores: HashMap<String, u32>,
    /// Kills per player slot (`player_slot -> kill_count`).
    pub player_kills: HashMap<i32, u32>,
    /// Damage dealt per player slot (`player_slot -> damage_dealt`).
    pub player_damage: HashMap<i32, f32>,
    /// Total rounds played.
    pub total_rounds: u32,
    /// Total kills recorded.
    pub total_kills: u32,
    /// Total damage recorded across all participants.
    pub total_damage: f32,
}

impl ReportSummary {
    /// Computes summary statistics from an event stream.
    pub fn compute_from_events(events: &[ReportEvent]) -> Self {
        let mut summary = Self::default();

        for ev in events {
            match ev {
                ReportEvent::Damage {
                    attacker, damage, ..
                } => {
                    summary.total_damage += damage;
                    if *attacker > 0 {
                        *summary.player_damage.entry(*attacker).or_insert(0.0) += damage;
                    }
                }
                ReportEvent::Kill { attacker, .. } => {
                    summary.total_kills += 1;
                    if *attacker > 0 {
                        *summary.player_kills.entry(*attacker).or_insert(0) += 1;
                    }
                }
                ReportEvent::RoundOutcome {
                    round_num,
                    winner_team,
                    ..
                } => {
                    summary.total_rounds = summary.total_rounds.max(*round_num);
                    if !winner_team.is_empty() {
                        *summary.team_scores.entry(winner_team.clone()).or_insert(0) += 1;
                    }
                }
                _ => {}
            }
        }

        summary
    }
}

/// Complete exported payload representation dispatched to exporters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportPayload {
    /// Unique report identifier.
    pub id: ReportId,
    /// Categorical report kind.
    pub kind: ReportKind,
    /// Unix timestamp in seconds when the report was initialized.
    pub created_at_epoch: u64,
    /// Unix timestamp in seconds when the report was closed/flushed.
    pub closed_at_epoch: u64,
    /// Current map name.
    pub map_name: String,
    /// Aggregated high-level metrics.
    pub summary: ReportSummary,
    /// Raw chronological event trace.
    pub events: Vec<ReportEvent>,
}
