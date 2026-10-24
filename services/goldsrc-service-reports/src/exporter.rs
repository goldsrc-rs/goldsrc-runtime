//! Service Provider Interface (SPI) and built-in exporters for reports.

use crate::model::ReportPayload;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Domain error encountered during report export.
#[derive(Debug, thiserror::Error)]
pub enum ReportExportError {
    #[error("Serialization failure: {0}")]
    Serialization(String),
    #[error("I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("Storage engine error: {0}")]
    Storage(String),
    #[error("Custom exporter failure: {0}")]
    Custom(String),
}

/// Pluggable export backend trait (SPI).
pub trait ReportExporter: Send + Sync {
    /// Human-readable identifier of the exporter.
    fn name(&self) -> &str;

    /// Dispatches a finalized report to the destination.
    fn export(&self, report: &ReportPayload) -> Result<(), ReportExportError>;
}

/// Filesystem JSON exporter writing atomic `.json` files into `<target_dir>/<report_id>.json`.
pub struct JsonFileExporter {
    target_dir: PathBuf,
}

impl JsonFileExporter {
    /// Creates a new `JsonFileExporter` targeting the given directory.
    pub fn new(target_dir: impl Into<PathBuf>) -> Self {
        Self {
            target_dir: target_dir.into(),
        }
    }
}

impl ReportExporter for JsonFileExporter {
    fn name(&self) -> &str {
        "json_file"
    }

    fn export(&self, report: &ReportPayload) -> Result<(), ReportExportError> {
        std::fs::create_dir_all(&self.target_dir)?;

        let json_text = serde_json::to_string_pretty(report)
            .map_err(|e| ReportExportError::Serialization(e.to_string()))?;

        let file_path = self.target_dir.join(format!("{}.json", report.id));
        let temp_path = self.target_dir.join(format!("{}.tmp", report.id));

        // Atomic file write: write to .tmp then rename
        std::fs::write(&temp_path, json_text.as_bytes())?;
        std::fs::rename(&temp_path, &file_path)?;

        log::info!(
            target: "goldsrc::reports",
            "[Reports] Exported report '{}' to file: {}",
            report.id,
            file_path.display()
        );

        Ok(())
    }
}

/// In-memory collector exporter for integration testing and live API queries.
#[derive(Default)]
pub struct MemoryCollectorExporter {
    collected: RwLock<Vec<ReportPayload>>,
}

impl MemoryCollectorExporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a clone of all collected reports.
    pub fn reports(&self) -> Vec<ReportPayload> {
        self.collected
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Returns the number of collected reports.
    pub fn len(&self) -> usize {
        self.collected
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Returns true if no reports have been collected.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ReportExporter for MemoryCollectorExporter {
    fn name(&self) -> &str {
        "memory_collector"
    }

    fn export(&self, report: &ReportPayload) -> Result<(), ReportExportError> {
        self.collected
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(report.clone());
        Ok(())
    }
}

#[cfg(feature = "storage")]
/// Exporter saving serialized reports directly into `goldsrc-service-storage`.
pub struct StorageExporter {
    storage: Arc<dyn goldsrc_service_storage::StoragePort>,
    bucket: String,
}

#[cfg(feature = "storage")]
impl StorageExporter {
    pub fn new(
        storage: Arc<dyn goldsrc_service_storage::StoragePort>,
        bucket: impl Into<String>,
    ) -> Self {
        Self {
            storage,
            bucket: bucket.into(),
        }
    }
}

#[cfg(feature = "storage")]
impl ReportExporter for StorageExporter {
    fn name(&self) -> &str {
        "sqlite_storage"
    }

    fn export(&self, report: &ReportPayload) -> Result<(), ReportExportError> {
        let json_bytes = serde_json::to_vec(report)
            .map_err(|e| ReportExportError::Serialization(e.to_string()))?;

        self.storage
            .set(&self.bucket, &report.id, &json_bytes)
            .map_err(|e| ReportExportError::Storage(e.to_string()))?;

        log::info!(
            target: "goldsrc::reports",
            "[Reports] Exported report '{}' to storage bucket '{}'",
            report.id,
            self.bucket
        );

        Ok(())
    }
}
