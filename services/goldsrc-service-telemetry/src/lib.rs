//! Host-side hardware metrics and system diagnostics service for GoldSrc.rs.
//!
//! Provides telemetry collection via `sysinfo` for CPU, process/system memory,
//! and engine tickrate/jitter monitoring.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use stitch_rs::flow::FlowControl;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, RefreshKind, System};

/// 64-byte aligned execution context for telemetry samples conforming to SMA.
#[repr(C, align(64))]
pub struct TelemetryContext<'a> {
    pub metrics: &'a mut SystemMetricsSnapshot,
    pub is_suppressed: bool,
}

impl<'a> stitch_rs::Blackboard for TelemetryContext<'a> {}

impl<'a> TelemetryContext<'a> {
    pub fn new(metrics: &'a mut SystemMetricsSnapshot) -> Self {
        Self {
            metrics,
            is_suppressed: false,
        }
    }
}

/// Middleware layer for intercepting, enriching, or auditing telemetry samples.
pub trait TelemetryLayer: Send + Sync {
    /// Descent phase: inspect or annotate metrics snapshot.
    fn on_enter(&self, ctx: &mut TelemetryContext<'_>) -> FlowControl<(), (), ()>;

    /// Ascent phase: audit logging, external telemetry fan-out.
    fn on_exit(&self, ctx: &mut TelemetryContext<'_>, outcome: &mut Result<(), ()>);
}

/// Terminal leaf node terminating telemetry U-cycle descent.
#[derive(Debug, Clone, Copy, Default)]
pub struct TelemetryTerminal;

impl stitch_rs::middleware::Terminal<TelemetryContext<'_>, (), (), ()> for TelemetryTerminal {
    #[inline(always)]
    fn execute(&mut self, ctx: &mut TelemetryContext<'_>, _intent: ()) -> Result<(), ()> {
        if ctx.is_suppressed { Err(()) } else { Ok(()) }
    }
}

/// Dynamic SMA telemetry pipeline orchestrator.
#[derive(Default, Clone)]
pub struct TelemetryPipeline {
    layers: Vec<Arc<dyn TelemetryLayer>>,
}

impl TelemetryPipeline {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    pub fn with_layer(mut self, layer: Arc<dyn TelemetryLayer>) -> Self {
        self.layers.push(layer);
        self
    }

    #[allow(clippy::result_unit_err)]
    pub fn dispatch(&self, ctx: &mut TelemetryContext<'_>) -> Result<(), ()> {
        if self.layers.is_empty() {
            return Ok(());
        }
        let mut entered_count = 0;
        let mut halted = false;

        for layer in &self.layers {
            match layer.on_enter(ctx) {
                FlowControl::Proceed(()) => {
                    entered_count += 1;
                }
                FlowControl::ShortCircuit(()) => {
                    entered_count += 1;
                    break;
                }
                FlowControl::Halt(()) => {
                    halted = true;
                    break;
                }
            }
        }

        let mut outcome = if halted || ctx.is_suppressed {
            Err(())
        } else {
            Ok(())
        };

        for layer in self.layers.iter().take(entered_count).rev() {
            layer.on_exit(ctx, &mut outcome);
        }

        outcome
    }
}

static TELEMETRY_PIPELINE: std::sync::LazyLock<RwLock<TelemetryPipeline>> =
    std::sync::LazyLock::new(|| RwLock::new(TelemetryPipeline::new()));

/// Registers a custom middleware layer with the global telemetry pipeline.
pub fn register_telemetry_layer(layer: Arc<dyn TelemetryLayer>) {
    let mut lock = TELEMETRY_PIPELINE
        .write()
        .unwrap_or_else(|e| e.into_inner());
    *lock = lock.clone().with_layer(layer);
}

/// Snapshot of host hardware and process performance metrics.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SystemMetricsSnapshot {
    /// Host CPU brand / model name (e.g. "Intel Core i7-10700K").
    pub cpu_brand: String,
    /// Host CPU vendor identifier (e.g. "GenuineIntel").
    pub cpu_vendor: String,
    /// Physical core count if detected.
    pub physical_cores: Option<usize>,
    /// Logical core (thread) count.
    pub logical_cores: usize,
    /// Global CPU utilization percentage (0.0 to 100.0).
    pub global_cpu_usage: f32,
    /// Process-specific CPU utilization percentage.
    pub process_cpu_usage: f32,
    /// Resident Set Size (RSS) memory used by the HLDS process in bytes.
    pub process_memory_rss_bytes: u64,
    /// Virtual memory used by the HLDS process in bytes.
    pub process_memory_virtual_bytes: u64,
    /// Total physical RAM on the host in bytes.
    pub total_memory_bytes: u64,
    /// Currently available physical RAM on the host in bytes.
    pub available_memory_bytes: u64,
    /// Total swap memory in bytes.
    pub total_swap_bytes: u64,
    /// Used swap memory in bytes.
    pub used_swap_bytes: u64,
    /// Measured engine tickrate in frames per second (FPS).
    pub measured_fps: f64,
    /// Frame time jitter standard deviation or range in milliseconds.
    pub frame_jitter_ms: f64,
    /// Uptime of the GoldSrc.rs runtime in seconds.
    pub uptime_secs: u64,
}

/// Rolling window frame time tracker for jitter and tickrate stability measurement.
#[derive(Debug)]
pub struct FrameTimeTracker {
    last_frame_instant: Option<Instant>,
    samples: Vec<f64>,
    sample_capacity: usize,
    sample_index: usize,
    total_samples: usize,
}

impl FrameTimeTracker {
    /// Creates a new frame time tracker with the specified sample buffer capacity.
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(16);
        Self {
            last_frame_instant: None,
            samples: vec![0.0; cap],
            sample_capacity: cap,
            sample_index: 0,
            total_samples: 0,
        }
    }

    /// Records the passage of an engine frame tick.
    pub fn record_frame(&mut self, now: Instant) {
        if let Some(prev) = self.last_frame_instant {
            let dt = now.duration_since(prev).as_secs_f64();
            if dt > 0.0 && dt < 2.0 {
                // Filter absurd delta spikes during level changes or debug freezes
                self.samples[self.sample_index] = dt;
                self.sample_index = (self.sample_index + 1) % self.sample_capacity;
                if self.total_samples < self.sample_capacity {
                    self.total_samples += 1;
                }
            }
        }
        self.last_frame_instant = Some(now);
    }

    /// Computes current average FPS and jitter (standard deviation of frame times in ms).
    pub fn calculate_fps_and_jitter(&self) -> (f64, f64) {
        if self.total_samples < 2 {
            return (0.0, 0.0);
        }
        let count = self.total_samples;
        let mut sum = 0.0;
        for i in 0..count {
            sum += self.samples[i];
        }
        let mean_dt = sum / (count as f64);
        if mean_dt <= 0.0 {
            return (0.0, 0.0);
        }
        let fps = 1.0 / mean_dt;

        // Compute standard deviation (jitter) in milliseconds
        let mut variance_sum = 0.0;
        for i in 0..count {
            let diff = (self.samples[i] - mean_dt) * 1000.0; // convert to ms
            variance_sum += diff * diff;
        }
        let jitter_ms = (variance_sum / (count as f64)).sqrt();
        (fps, jitter_ms)
    }
}

/// Host System Information Service collecting CPU, RAM, and tickrate telemetry.
pub struct SystemInfoService {
    system: Mutex<System>,
    pid: Pid,
    start_time: Instant,
    frame_tracker: Mutex<FrameTimeTracker>,
}

impl Default for SystemInfoService {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemInfoService {
    /// Creates and initializes a new `SystemInfoService`.
    pub fn new() -> Self {
        let refresh_kind = RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything());
        let mut system = System::new_with_specifics(refresh_kind);
        system.refresh_cpu_all();
        system.refresh_memory();

        let pid = Pid::from_u32(std::process::id());
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );

        Self {
            system: Mutex::new(system),
            pid,
            start_time: Instant::now(),
            frame_tracker: Mutex::new(FrameTimeTracker::new(128)),
        }
    }

    /// Notifies the service that a new frame has completed. Call from engine frame hook (`StartFrame`).
    pub fn on_frame(&self) {
        if let Ok(mut tracker) = self.frame_tracker.lock() {
            tracker.record_frame(Instant::now());
        }
    }

    /// Captures a fresh snapshot of all system and process telemetry.
    pub fn sample_metrics(&self) -> SystemMetricsSnapshot {
        let mut sys = self.system.lock().unwrap_or_else(|p| p.into_inner());

        sys.refresh_cpu_all();
        sys.refresh_memory();
        sys.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );

        let cpus = sys.cpus();
        let (cpu_brand, cpu_vendor) = if let Some(cpu) = cpus.first() {
            (cpu.brand().to_string(), cpu.vendor_id().to_string())
        } else {
            ("Unknown CPU".to_string(), "Unknown Vendor".to_string())
        };

        let physical_cores = sysinfo::System::physical_core_count();
        let logical_cores = cpus.len();
        let global_cpu_usage = sys.global_cpu_usage();

        let (proc_cpu, proc_rss, proc_virt) = if let Some(proc) = sys.process(self.pid) {
            (proc.cpu_usage(), proc.memory(), proc.virtual_memory())
        } else {
            (0.0, 0, 0)
        };

        let total_mem = sys.total_memory();
        let avail_mem = sys.available_memory();
        let total_swap = sys.total_swap();
        let used_swap = sys.used_swap();

        let (measured_fps, frame_jitter_ms) = self
            .frame_tracker
            .lock()
            .map(|t| t.calculate_fps_and_jitter())
            .unwrap_or((0.0, 0.0));

        let uptime_secs = self.start_time.elapsed().as_secs();

        let mut snapshot = SystemMetricsSnapshot {
            cpu_brand,
            cpu_vendor,
            physical_cores,
            logical_cores,
            global_cpu_usage,
            process_cpu_usage: proc_cpu,
            process_memory_rss_bytes: proc_rss,
            process_memory_virtual_bytes: proc_virt,
            total_memory_bytes: total_mem,
            available_memory_bytes: avail_mem,
            total_swap_bytes: total_swap,
            used_swap_bytes: used_swap,
            measured_fps,
            frame_jitter_ms,
            uptime_secs,
        };

        let mut ctx = TelemetryContext::new(&mut snapshot);
        let lock = TELEMETRY_PIPELINE.read().unwrap_or_else(|e| e.into_inner());
        let _ = lock.dispatch(&mut ctx);

        snapshot
    }
}

static SYSTEM_INFO_SERVICE: std::sync::OnceLock<SystemInfoService> = std::sync::OnceLock::new();

/// Global singleton access to the host `SystemInfoService`.
pub fn system_info() -> &'static SystemInfoService {
    SYSTEM_INFO_SERVICE.get_or_init(SystemInfoService::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_system_info_snapshot_contains_valid_hardware_data() {
        let service = SystemInfoService::new();
        // Give sysinfo a brief sleep or repeated sample for cpu calculation
        std::thread::sleep(Duration::from_millis(10));
        let snapshot = service.sample_metrics();

        assert!(!snapshot.cpu_brand.is_empty() || snapshot.logical_cores > 0);
        assert!(snapshot.total_memory_bytes > 0);
    }

    #[test]
    fn test_frame_tracker_fps_and_jitter_calculation() {
        let mut tracker = FrameTimeTracker::new(32);
        let start = Instant::now();

        // Simulate 20 frames with ~10ms interval (100 FPS)
        for i in 0..20 {
            let t = start + Duration::from_millis(i * 10);
            tracker.record_frame(t);
        }

        let (fps, jitter) = tracker.calculate_fps_and_jitter();
        assert!(
            (90.0..=110.0).contains(&fps),
            "Expected ~100 FPS, got {fps}"
        );
        assert!(
            (0.0..1.0).contains(&jitter),
            "Expected low jitter, got {jitter}ms"
        );
    }

    #[test]
    fn test_telemetry_context_alignment() {
        let mut snapshot = SystemMetricsSnapshot {
            cpu_brand: "CPU".to_string(),
            cpu_vendor: "Vendor".to_string(),
            physical_cores: Some(8),
            logical_cores: 16,
            global_cpu_usage: 10.0,
            process_cpu_usage: 5.0,
            process_memory_rss_bytes: 1024,
            process_memory_virtual_bytes: 2048,
            total_memory_bytes: 4096,
            available_memory_bytes: 2048,
            total_swap_bytes: 0,
            used_swap_bytes: 0,
            measured_fps: 100.0,
            frame_jitter_ms: 0.1,
            uptime_secs: 100,
        };
        let ctx = TelemetryContext::new(&mut snapshot);
        assert_eq!(std::mem::align_of_val(&ctx), 64);
        assert!(!ctx.is_suppressed);
    }

    struct MockTelemetryEnricher;

    impl TelemetryLayer for MockTelemetryEnricher {
        fn on_enter(&self, ctx: &mut TelemetryContext<'_>) -> FlowControl<(), (), ()> {
            ctx.metrics.global_cpu_usage = 99.9;
            FlowControl::Proceed(())
        }

        fn on_exit(&self, _ctx: &mut TelemetryContext<'_>, _outcome: &mut Result<(), ()>) {}
    }

    #[test]
    fn test_telemetry_pipeline_u_cycle() {
        let pipeline = TelemetryPipeline::new().with_layer(Arc::new(MockTelemetryEnricher));
        let mut snapshot = SystemMetricsSnapshot {
            cpu_brand: "CPU".to_string(),
            cpu_vendor: "Vendor".to_string(),
            physical_cores: Some(8),
            logical_cores: 16,
            global_cpu_usage: 10.0,
            process_cpu_usage: 5.0,
            process_memory_rss_bytes: 1024,
            process_memory_virtual_bytes: 2048,
            total_memory_bytes: 4096,
            available_memory_bytes: 2048,
            total_swap_bytes: 0,
            used_swap_bytes: 0,
            measured_fps: 100.0,
            frame_jitter_ms: 0.1,
            uptime_secs: 100,
        };

        let mut ctx = TelemetryContext::new(&mut snapshot);
        let res = pipeline.dispatch(&mut ctx);
        assert_eq!(res, Ok(()));
        assert_eq!(snapshot.global_cpu_usage, 99.9);
    }
}
