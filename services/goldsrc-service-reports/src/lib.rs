//! # GoldSrc Reports Service (`goldsrc-service-reports`)
//!
//! Provides deterministic and scheduled batch report aggregation for GoldSrc.rs:
//! - Aggregates events across match sessions, rounds, players, and server health.
//! - **Dual-Trigger Execution**:
//!   1. **Deterministic Trigger**: Explicit `finish_and_flush(id)` invoked when the system or plugin
//!      knows a match/scrim has finalized.
//!   2. **Periodic Trigger**: Automatic expiry and flushing on timers or map transitions.
//! - **Pluggable Exporter SPI**:
//!   - `JsonFileExporter`: Atomic filesystem serialization into `<logs>/reports/<id>.json`.
//!   - `StorageExporter`: Direct ingestion into `goldsrc-service-storage` SQLite WAL bucket `"reports"`.
//!   - `MemoryCollectorExporter`: In-memory queryable buffer for unit tests and telemetry APIs.

pub mod engine;
pub mod exporter;
pub mod model;

pub use engine::ReportAggregator;
#[cfg(feature = "storage")]
pub use exporter::StorageExporter;
pub use exporter::{JsonFileExporter, MemoryCollectorExporter, ReportExportError, ReportExporter};
pub use model::{ReportEvent, ReportId, ReportKind, ReportPayload, ReportStatus, ReportSummary};

use std::sync::{Arc, LazyLock, RwLock};
use stitch_rs::define_pipeline;
pub use stitch_rs::flow::FlowControl;

/// 64-byte aligned execution context for report event submissions and flushing under SMA.
#[repr(C, align(64))]
pub struct ReportSubmissionContext<'a> {
    pub report_id: &'a str,
    pub event_count: usize,
    pub is_suppressed: bool,
    pub suppression_reason: Option<String>,
}

impl<'a> stitch_rs::Blackboard for ReportSubmissionContext<'a> {}

impl<'a> ReportSubmissionContext<'a> {
    pub fn new(report_id: &'a str, event_count: usize) -> Self {
        Self {
            report_id,
            event_count,
            is_suppressed: false,
            suppression_reason: None,
        }
    }

    pub fn suppress(&mut self, reason: impl Into<String>) {
        self.is_suppressed = true;
        self.suppression_reason = Some(reason.into());
    }
}

/// The intent entering report submission.
#[derive(Debug, Clone, PartialEq)]
pub enum ReportIntent<'a> {
    RecordEvent { id: &'a str, event: &'a ReportEvent },
    Flush { id: &'a str },
}

define_pipeline! {
    name: Report,
    context: ReportSubmissionContext<'_>,
    intent: ReportIntent<'_>,
    outcome: (),
    error: (),
    vis: pub,
}

static REPORT_PIPELINE: LazyLock<RwLock<ReportPipeline>> =
    LazyLock::new(|| RwLock::new(ReportPipeline::new()));

/// Registers a custom middleware layer with the global report pipeline.
pub fn register_report_layer(layer: Arc<dyn ReportLayer>) {
    let mut lock = REPORT_PIPELINE.write().unwrap_or_else(|e| e.into_inner());
    *lock = lock.clone().with_layer(layer);
}

/// Dispatches a report action through the global SMA U-cycle pipeline.
#[allow(clippy::result_unit_err)]
pub fn dispatch_report_pipeline(
    ctx: &mut ReportSubmissionContext<'_>,
    intent: &ReportIntent<'_>,
) -> Result<(), ()> {
    let lock = REPORT_PIPELINE.read().unwrap_or_else(|e| e.into_inner());
    lock.dispatch(ctx, intent)
}

static GLOBAL_REPORTS: LazyLock<Arc<ReportAggregator>> =
    LazyLock::new(|| Arc::new(ReportAggregator::new()));

/// Returns a shared reference to the global `ReportAggregator` instance.
pub fn global_reports() -> Arc<ReportAggregator> {
    Arc::clone(&GLOBAL_REPORTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic_report_flow() {
        let aggregator = ReportAggregator::new();
        let collector = Arc::new(MemoryCollectorExporter::new());
        aggregator.register_exporter(Arc::clone(&collector) as Arc<dyn ReportExporter>);

        // 1. Start match report
        assert!(aggregator.start_report("match_scrim_01", ReportKind::MatchSession, "de_dust2"));
        // Duplicate start returns false
        assert!(!aggregator.start_report("match_scrim_01", ReportKind::MatchSession, "de_dust2"));

        assert_eq!(aggregator.active_count(), 1);

        // 2. Record telemetry events
        aggregator.record_event(
            "match_scrim_01",
            ReportEvent::Damage {
                attacker: 1,
                victim: 2,
                damage: 85.5,
                weapon: "weapon_ak47".to_string(),
                hitgroup: 1,
                timestamp_ms: 1000,
            },
        );
        aggregator.record_event(
            "match_scrim_01",
            ReportEvent::Kill {
                attacker: 1,
                victim: 2,
                weapon: "weapon_ak47".to_string(),
                headshot: true,
                timestamp_ms: 1050,
            },
        );
        aggregator.record_event(
            "match_scrim_01",
            ReportEvent::RoundOutcome {
                round_num: 1,
                winner_team: "TERRORIST".to_string(),
                duration_secs: 45.2,
                reason: "Target Bombed".to_string(),
            },
        );

        // 3. Deterministic flush: system knows match ended
        let payload = aggregator.finish_and_flush("match_scrim_01").unwrap();

        assert_eq!(payload.id, "match_scrim_01");
        assert_eq!(payload.summary.total_kills, 1);
        assert_eq!(payload.summary.total_damage, 85.5);
        assert_eq!(payload.summary.total_rounds, 1);
        assert_eq!(payload.summary.team_scores.get("TERRORIST"), Some(&1));
        assert_eq!(payload.summary.player_kills.get(&1), Some(&1));

        // 4. Verify exporter received the payload
        assert_eq!(collector.len(), 1);
        let exported = &collector.reports()[0];
        assert_eq!(exported.id, "match_scrim_01");
        assert_eq!(exported.events.len(), 3);

        // Active report has been removed
        assert_eq!(aggregator.active_count(), 0);
        // Double flush fails gracefully
        assert!(aggregator.finish_and_flush("match_scrim_01").is_err());
    }

    #[test]
    fn test_global_event_broadcast_and_periodic_expiry() {
        let aggregator = ReportAggregator::new();
        let collector = Arc::new(MemoryCollectorExporter::new());
        aggregator.register_exporter(Arc::clone(&collector) as Arc<dyn ReportExporter>);

        assert!(aggregator.start_report("periodic_01", ReportKind::ServerHealth, "de_inferno"));
        assert!(aggregator.start_report("periodic_02", ReportKind::PlayerActivity, "de_inferno"));

        // Global event broadcasts to all active reports
        aggregator.record_global_event(ReportEvent::RoundOutcome {
            round_num: 1,
            winner_team: "CT".to_string(),
            duration_secs: 60.0,
            reason: "All Terrorists Eliminated".to_string(),
        });

        // Both reports have 1 event
        // Simulate immediate expiry with max_age_secs = 0
        let flushed = aggregator.check_periodic_expiry(0);
        assert_eq!(flushed.len(), 2);
        assert_eq!(collector.len(), 2);
        assert_eq!(aggregator.active_count(), 0);
    }

    #[test]
    fn test_json_file_exporter_atomic_write() {
        let temp_dir =
            std::env::temp_dir().join(format!("goldsrc_test_reports_{}", std::process::id()));
        let exporter = JsonFileExporter::new(&temp_dir);

        let report = ReportPayload {
            id: "test_report_file".to_string(),
            kind: ReportKind::RoundTelemetry,
            created_at_epoch: 1000,
            closed_at_epoch: 1050,
            map_name: "de_nuke".to_string(),
            summary: ReportSummary {
                total_rounds: 3,
                ..Default::default()
            },
            events: vec![ReportEvent::RoundOutcome {
                round_num: 3,
                winner_team: "CT".to_string(),
                duration_secs: 55.0,
                reason: "Bomb Defused".to_string(),
            }],
        };

        exporter.export(&report).unwrap();

        let target_file = temp_dir.join("test_report_file.json");
        assert!(target_file.exists());

        let content = std::fs::read_to_string(&target_file).unwrap();
        assert!(content.contains("\"test_report_file\""));
        assert!(content.contains("\"de_nuke\""));

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[cfg(feature = "storage")]
    #[test]
    fn test_sqlite_storage_exporter_integration() {
        use goldsrc_service_storage::{SqliteStorageEngine, StoragePort};

        let unique_id = format!("goldsrc_test_storage_reports_{}", std::process::id());
        let temp_dir = std::env::temp_dir().join(unique_id);
        let db_file = temp_dir.join("reports_test.db");

        let storage = SqliteStorageEngine::open(&db_file).unwrap();
        let exporter =
            StorageExporter::new(Arc::clone(&storage) as Arc<dyn StoragePort>, "reports");

        let report = ReportPayload {
            id: "db_report_01".to_string(),
            kind: ReportKind::MatchSession,
            created_at_epoch: 500,
            closed_at_epoch: 600,
            map_name: "de_mirage".to_string(),
            summary: ReportSummary {
                total_kills: 42,
                ..Default::default()
            },
            events: vec![],
        };

        exporter.export(&report).unwrap();

        // Read back from storage engine
        let bytes = storage.get("reports", "db_report_01").unwrap().unwrap();
        let decoded: ReportPayload = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.id, "db_report_01");
        assert_eq!(decoded.summary.total_kills, 42);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_report_submission_context_alignment() {
        let ctx = ReportSubmissionContext::new("rep_01", 10);
        assert_eq!(std::mem::align_of_val(&ctx), 64);
        assert!(!ctx.is_suppressed);
    }

    struct MockReportFilterLayer;

    impl ReportLayer for MockReportFilterLayer {
        fn on_enter(
            &self,
            ctx: &mut ReportSubmissionContext<'_>,
            intent: &ReportIntent<'_>,
        ) -> FlowControl<(), (), ()> {
            if let ReportIntent::RecordEvent { id, .. } = intent
                && *id == "blocked_report"
            {
                ctx.suppress("Blacklisted report id");
                return FlowControl::Halt(());
            }
            FlowControl::Proceed(())
        }

        fn on_exit(
            &self,
            _ctx: &mut ReportSubmissionContext<'_>,
            _intent: &ReportIntent<'_>,
            _outcome: &mut Result<(), ()>,
        ) {
        }
    }

    #[test]
    fn test_report_pipeline_u_cycle() {
        let pipeline = ReportPipeline::new().with_layer(Arc::new(MockReportFilterLayer));

        let mut ctx = ReportSubmissionContext::new("blocked_report", 1);
        let dummy_event = ReportEvent::RoundOutcome {
            round_num: 1,
            winner_team: "CT".to_string(),
            duration_secs: 105.0,
            reason: "Target eliminated".to_string(),
        };
        let intent = ReportIntent::RecordEvent {
            id: "blocked_report",
            event: &dummy_event,
        };

        let res = pipeline.dispatch(&mut ctx, &intent);
        assert_eq!(res, Err(()));
        assert!(ctx.is_suppressed);
    }
}
