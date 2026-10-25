//! Reports service integration and log consolidation coordinator.

use goldsrc_service_reports::{JsonFileExporter, ReportAggregator, ReportEvent, ReportKind};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

static AGGREGATOR: OnceLock<Arc<ReportAggregator>> = OnceLock::new();
static CURRENT_REPORT_ID: RwLock<Option<String>> = RwLock::new(None);

/// Global accessor for the report aggregator.
pub fn report_aggregator() -> Arc<ReportAggregator> {
    AGGREGATOR
        .get_or_init(|| Arc::new(ReportAggregator::new()))
        .clone()
}

/// Initializes the reports service with a base logging directory.
pub fn init_reports(logs_dir: PathBuf) {
    let agg = report_aggregator();
    let reports_dir = logs_dir.join("reports");
    let _ = std::fs::create_dir_all(&reports_dir);
    agg.register_exporter(Arc::new(JsonFileExporter::new(reports_dir)));
}

/// Starts an aggregated report session for a map.
pub fn start_map_session(map_name: &str) {
    let agg = report_aggregator();
    let report_id = format!(
        "session_{}_{}",
        map_name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );

    agg.start_report(&report_id, ReportKind::MatchSession, map_name);

    if let Ok(mut cur) = CURRENT_REPORT_ID.write() {
        *cur = Some(report_id);
    }
}

/// Records a gameplay or security event into the active report session.
pub fn record_event(event: ReportEvent) {
    let agg = report_aggregator();
    if let Ok(cur) = CURRENT_REPORT_ID.read()
        && let Some(ref id) = *cur
    {
        agg.record_event(id, event);
    }
}

/// Finalizes and flushes the active report session to disk.
pub fn finalize_map_session() {
    let agg = report_aggregator();
    let active_id = {
        let mut cur = CURRENT_REPORT_ID.write().unwrap_or_else(|e| e.into_inner());
        cur.take()
    };

    if let Some(id) = active_id {
        let _ = agg.finish_and_flush(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goldsrc_service_reports::ReportEvent;

    #[test]
    fn test_map_session_lifecycle_and_event_recording() {
        let temp_dir =
            std::env::temp_dir().join(format!("goldsrc_test_reports_{}", std::process::id()));
        init_reports(temp_dir.clone());

        start_map_session("de_dust2");

        record_event(ReportEvent::Kill {
            attacker: 1,
            victim: 2,
            weapon: "ak47".to_string(),
            headshot: true,
            timestamp_ms: 1000,
        });

        record_event(ReportEvent::Damage {
            attacker: 1,
            victim: 2,
            damage: 85.0,
            weapon: "ak47".to_string(),
            hitgroup: 1,
            timestamp_ms: 950,
        });

        finalize_map_session();

        let reports_dir = temp_dir.join("reports");
        let entries = std::fs::read_dir(&reports_dir);
        assert!(entries.is_ok(), "reports dir must exist");

        let files: Vec<_> = entries
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .collect();

        assert!(
            !files.is_empty(),
            "a json report file must have been flushed"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
