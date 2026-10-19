//! Core report aggregation and dual-trigger dispatch engine.

use crate::exporter::{ReportExportError, ReportExporter};
use crate::model::{ReportEvent, ReportId, ReportKind, ReportPayload, ReportStatus, ReportSummary};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Internal record of an open, actively accumulating report.
struct ActiveReport {
    kind: ReportKind,
    created_at_epoch: u64,
    map_name: String,
    status: ReportStatus,
    events: Vec<ReportEvent>,
}

/// Central aggregator and dual-trigger dispatcher for GoldSrc reports.
pub struct ReportAggregator {
    active_reports: RwLock<HashMap<ReportId, ActiveReport>>,
    exporters: RwLock<Vec<Arc<dyn ReportExporter>>>,
}

impl Default for ReportAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl ReportAggregator {
    /// Creates a new `ReportAggregator`.
    pub fn new() -> Self {
        Self {
            active_reports: RwLock::new(HashMap::new()),
            exporters: RwLock::new(Vec::new()),
        }
    }

    /// Registers a pluggable report exporter.
    pub fn register_exporter(&self, exporter: Arc<dyn ReportExporter>) {
        self.exporters
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(exporter);
    }

    /// Starts a new active report. Returns `false` if a report with this ID is already active.
    pub fn start_report(
        &self,
        id: impl Into<ReportId>,
        kind: ReportKind,
        map_name: impl Into<String>,
    ) -> bool {
        let id = id.into();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut reports = self
            .active_reports
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if reports.contains_key(&id) {
            return false;
        }

        reports.insert(
            id,
            ActiveReport {
                kind,
                created_at_epoch: now,
                map_name: map_name.into(),
                status: ReportStatus::Active,
                events: Vec::new(),
            },
        );
        true
    }

    /// Appends a structured telemetry event to a specific active report.
    pub fn record_event(&self, id: &str, event: ReportEvent) -> bool {
        let mut ctx = crate::ReportSubmissionContext::new(id, 1);
        let intent = crate::ReportIntent::RecordEvent { id, event: &event };

        if crate::dispatch_report_pipeline(&mut ctx, &intent).is_err() {
            return false;
        }

        let mut reports = self
            .active_reports
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(report) = reports.get_mut(id)
            && report.status == ReportStatus::Active
        {
            report.events.push(event);
            return true;
        }
        false
    }

    /// Appends a structured telemetry event to all currently active reports.
    pub fn record_global_event(&self, event: ReportEvent) {
        let mut reports = self
            .active_reports
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for report in reports.values_mut() {
            if report.status == ReportStatus::Active {
                report.events.push(event.clone());
            }
        }
    }

    /// **Deterministic Trigger**: Completes and immediately flushes a report.
    ///
    /// Computes summary statistics, sends payload to all registered exporters,
    /// and removes the report from active tracking.
    pub fn finish_and_flush(&self, id: &str) -> Result<ReportPayload, ReportExportError> {
        let mut ctx = crate::ReportSubmissionContext::new(id, 0);
        let intent = crate::ReportIntent::Flush { id };

        if crate::dispatch_report_pipeline(&mut ctx, &intent).is_err() {
            return Err(ReportExportError::Custom(format!(
                "Report flush '{id}' was suppressed by policy"
            )));
        }

        let active = {
            let mut reports = self
                .active_reports
                .write()
                .unwrap_or_else(|e| e.into_inner());
            reports.remove(id).ok_or_else(|| {
                ReportExportError::Custom(format!("Active report with id '{id}' not found"))
            })?
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let summary = ReportSummary::compute_from_events(&active.events);

        let payload = ReportPayload {
            id: id.to_string(),
            kind: active.kind,
            created_at_epoch: active.created_at_epoch,
            closed_at_epoch: now,
            map_name: active.map_name,
            summary,
            events: active.events,
        };

        // Dispatch to all registered exporters
        let exporters = self.exporters.read().unwrap_or_else(|e| e.into_inner());
        for exp in exporters.iter() {
            if let Err(err) = exp.export(&payload) {
                log::error!(
                    target: "goldsrc::reports",
                    "[Reports] Exporter '{}' failed for report '{}': {err}",
                    exp.name(),
                    payload.id
                );
            }
        }

        Ok(payload)
    }

    /// Flushes all currently active reports (e.g. on map change or server shutdown).
    pub fn flush_all_active(&self) -> Vec<ReportPayload> {
        let ids: Vec<ReportId> = {
            let reports = self
                .active_reports
                .read()
                .unwrap_or_else(|e| e.into_inner());
            reports.keys().cloned().collect()
        };

        let mut results = Vec::new();
        for id in ids {
            if let Ok(payload) = self.finish_and_flush(&id) {
                results.push(payload);
            }
        }
        results
    }

    /// **Periodic Trigger**: Checks for reports that have been active longer than `max_age_secs`
    /// and automatically flushes them.
    pub fn check_periodic_expiry(&self, max_age_secs: u64) -> Vec<ReportPayload> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let expired_ids: Vec<ReportId> = {
            let reports = self
                .active_reports
                .read()
                .unwrap_or_else(|e| e.into_inner());
            reports
                .iter()
                .filter(|(_, r)| now.saturating_sub(r.created_at_epoch) >= max_age_secs)
                .map(|(id, _)| id.clone())
                .collect()
        };

        let mut flushed = Vec::new();
        for id in expired_ids {
            if let Ok(payload) = self.finish_and_flush(&id) {
                flushed.push(payload);
            }
        }
        flushed
    }

    /// Returns the number of currently active reports.
    pub fn active_count(&self) -> usize {
        self.active_reports
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}
