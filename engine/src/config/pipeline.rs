//! Configuration model for the console pipeline preprocessor (`|`, `&&`, `>`, `<`).
//!
//! Provides granular policy controls partitioned across three execution domains:
//! - Server Console (trusted host admin / RCON)
//! - Client Console (`ClientCommand` from connected players)
//! - In-Game Chat (`say`, `say_team`)

use serde::{Deserialize, Serialize};

/// Granular permissions and capabilities for a specific command execution domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineDomainPolicy {
    /// Enable pipe streaming operator (`|`).
    #[serde(default)]
    pub pipes: bool,

    /// Enable conditional chaining operator (`&&`).
    #[serde(default)]
    pub chaining: bool,

    /// Enable output redirection to file (`>` and `>>`).
    #[serde(default)]
    pub redirection: bool,

    /// Enable input redirection from file (`<`).
    #[serde(default)]
    pub input_redirection: bool,
}

impl PipelineDomainPolicy {
    /// Default policy for trusted server console (all features permitted).
    pub const fn server_default() -> Self {
        Self {
            pipes: true,
            chaining: true,
            redirection: true,
            input_redirection: true,
        }
    }

    /// Default policy for semi-trusted client console (only chaining `&&` permitted for client binds).
    pub const fn client_console_default() -> Self {
        Self {
            pipes: false,
            chaining: true,
            redirection: false,
            input_redirection: false,
        }
    }

    /// Default policy for public in-game chat (all pipeline syntax disabled by default).
    pub const fn chat_default() -> Self {
        Self {
            pipes: false,
            chaining: false,
            redirection: false,
            input_redirection: false,
        }
    }
}

/// Pipeline preprocessor configuration in `goldsrc.toml` under `[console.pipeline]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolePipelineConfig {
    /// Master switch for the console pipeline preprocessor.
    #[serde(default = "default_pipeline_enabled")]
    pub enabled: bool,

    /// Maximum pipe depth allowed in a single chained line (DoS/recursion prevention).
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,

    /// Maximum buffer size in bytes for piped output.
    #[serde(default = "default_max_buffer_bytes")]
    pub max_buffer_bytes: usize,

    /// Server console policy.
    #[serde(default = "PipelineDomainPolicy::server_default")]
    pub server: PipelineDomainPolicy,

    /// Client console (`ClientCommand`) policy.
    #[serde(default = "PipelineDomainPolicy::client_console_default")]
    pub client_console: PipelineDomainPolicy,

    /// In-game chat (`say` / `say_team`) policy.
    #[serde(default = "PipelineDomainPolicy::chat_default")]
    pub chat: PipelineDomainPolicy,
}

const fn default_pipeline_enabled() -> bool {
    true
}

const fn default_max_depth() -> usize {
    8
}

const fn default_max_buffer_bytes() -> usize {
    4096
}

impl Default for ConsolePipelineConfig {
    fn default() -> Self {
        Self {
            enabled: default_pipeline_enabled(),
            max_depth: default_max_depth(),
            max_buffer_bytes: default_max_buffer_bytes(),
            server: PipelineDomainPolicy::server_default(),
            client_console: PipelineDomainPolicy::client_console_default(),
            chat: PipelineDomainPolicy::chat_default(),
        }
    }
}
