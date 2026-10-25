//! Structured command status and formatted CLI responses.

/// Status of a CLI command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    /// Operation succeeded and desired state was achieved.
    Success,
    /// Operation was valid, but state was already as requested (no-op / notice).
    Notice,
    /// Operation succeeded with caveats or a non-fatal warning.
    Warning,
    /// Operation failed due to invalid arguments, missing resources, or runtime errors.
    Error,
}

impl CommandStatus {
    /// Returns the console prefix tag for this status.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Success => "[GoldSrc.rs][OK]",
            Self::Notice => "[GoldSrc.rs][NOTE]",
            Self::Warning => "[GoldSrc.rs][WARN]",
            Self::Error => "[GoldSrc.rs][ERROR]",
        }
    }
}

/// Structured response from a host CLI command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliResponse {
    /// Result status.
    pub status: CommandStatus,
    /// Human-readable message.
    pub message: String,
}

impl CliResponse {
    /// Creates a success response.
    pub fn success(msg: impl Into<String>) -> Self {
        Self {
            status: CommandStatus::Success,
            message: msg.into(),
        }
    }

    /// Creates a notice / idempotent no-op response.
    pub fn notice(msg: impl Into<String>) -> Self {
        Self {
            status: CommandStatus::Notice,
            message: msg.into(),
        }
    }

    /// Creates a warning response.
    pub fn warning(msg: impl Into<String>) -> Self {
        Self {
            status: CommandStatus::Warning,
            message: msg.into(),
        }
    }

    /// Creates an error response.
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            status: CommandStatus::Error,
            message: msg.into(),
        }
    }

    /// Formats the response for printing to the server console.
    pub fn format_console(&self) -> String {
        format!("{} {}\n", self.status.tag(), self.message)
    }
}

impl std::fmt::Display for CliResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.status.tag(), self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_response_formatting() {
        let ok = CliResponse::success("Plugin 'vip_menu' paused successfully.");
        assert_eq!(ok.status, CommandStatus::Success);
        assert_eq!(
            ok.format_console(),
            "[GoldSrc.rs][OK] Plugin 'vip_menu' paused successfully.\n"
        );

        let note = CliResponse::notice("Plugin 'vip_menu' is already paused.");
        assert_eq!(note.status, CommandStatus::Notice);
        assert_eq!(
            note.format_console(),
            "[GoldSrc.rs][NOTE] Plugin 'vip_menu' is already paused.\n"
        );

        let warn = CliResponse::warning("Command 'vip' unhandled.");
        assert_eq!(warn.status, CommandStatus::Warning);
        assert_eq!(
            warn.format_console(),
            "[GoldSrc.rs][WARN] Command 'vip' unhandled.\n"
        );

        let err = CliResponse::error("plugin index 99 out of bounds");
        assert_eq!(err.status, CommandStatus::Error);
        assert_eq!(
            err.format_console(),
            "[GoldSrc.rs][ERROR] plugin index 99 out of bounds\n"
        );
    }
}
