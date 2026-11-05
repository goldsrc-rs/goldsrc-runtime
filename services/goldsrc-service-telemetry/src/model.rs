//! Data models for host telemetry: separating cold static host metadata
//! from high-frequency tick telemetry packets.

/// Static hardware and host architecture metadata (cold path).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HostHardwareInfo {
    /// Host CPU brand / model name (e.g. "Intel Core i7-10700K").
    pub cpu_brand: String,
    /// Host CPU vendor identifier (e.g. "GenuineIntel").
    pub cpu_vendor: String,
    /// Physical core count if detected.
    pub physical_cores: Option<usize>,
    /// Logical core (thread) count.
    pub logical_cores: usize,
    /// Total physical RAM on the host in bytes.
    pub total_memory_bytes: u64,
    /// Operating system name / version descriptor.
    pub os_name: String,
}

impl HostHardwareInfo {
    /// Creates a new `HostHardwareInfo` descriptor.
    pub fn new(
        cpu_brand: impl Into<String>,
        cpu_vendor: impl Into<String>,
        physical_cores: Option<usize>,
        logical_cores: usize,
        total_memory_bytes: u64,
        os_name: impl Into<String>,
    ) -> Self {
        Self {
            cpu_brand: cpu_brand.into(),
            cpu_vendor: cpu_vendor.into(),
            physical_cores,
            logical_cores,
            total_memory_bytes,
            os_name: os_name.into(),
        }
    }
}
