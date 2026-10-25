//! Unified structured logger for GoldSrc.rs backends.

use crate::paths::{BackendType, PathResolver};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU8, Ordering},
    },
};

static ATOMIC_MAX_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info.as_u8());

// ============================================================================
// Configuration (matches [logging] section in goldsrc.toml)
// ============================================================================

/// Minimum log level. Messages below this level are silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Most verbose; messages for fine-grained tracing.
    Trace,
    /// Debug diagnostics.
    Debug,
    /// Normal operational messages.
    Info,
    /// Warnings that do not stop execution.
    Warn,
    /// Errors that may indicate failure.
    Error,
}

impl LogLevel {
    /// Returns the fixed-width tag used in log lines (e.g. `"INFO "`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    /// Convert from `log::Level`
    pub fn from_log_level(level: log::Level) -> Self {
        match level {
            log::Level::Trace => Self::Trace,
            log::Level::Debug => Self::Debug,
            log::Level::Info => Self::Info,
            log::Level::Warn => Self::Warn,
            log::Level::Error => Self::Error,
        }
    }

    /// Convert to `log::LevelFilter`
    pub fn to_level_filter(self) -> log::LevelFilter {
        match self {
            Self::Trace => log::LevelFilter::Trace,
            Self::Debug => log::LevelFilter::Debug,
            Self::Info => log::LevelFilter::Info,
            Self::Warn => log::LevelFilter::Warn,
            Self::Error => log::LevelFilter::Error,
        }
    }

    /// Convert to numeric representation for atomic operations.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Trace => 1,
            Self::Debug => 2,
            Self::Info => 3,
            Self::Warn => 4,
            Self::Error => 5,
        }
    }

    /// Convert from numeric representation.
    pub const fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Trace,
            2 => Self::Debug,
            4 => Self::Warn,
            5 => Self::Error,
            _ => Self::Info,
        }
    }
}

use goldsrc_api::consts::log_targets;

/// Logical sub-system producing the log message. Enables per-target filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogTarget {
    /// Framework core (init, lifecycle, config).
    Core,
    /// Authentication, capabilities, and permissions.
    Auth,
    /// Persistence, SQLite, and KV storage engine.
    Storage,
    /// Declarative reactive rules engine.
    Rules,
    /// Menu presentation and user sessions.
    Menu,
    /// Internationalization dictionary compiler and placeholder expansion.
    I18n,
    /// Entity Component System (ECS) world, stages, and systems.
    Ecs,
    /// WASM host and plugin management.
    Wasm,
    /// GameDLL proxy layer (standalone backend).
    Proxy,
    /// Metamod engine interface bridge and precache manager.
    Engine,
    /// ReHLDS/ReGameDLL interface bindings.
    Reapi,
    /// Filesystem watcher service and hot-reload debouncer.
    Watcher,
    /// Core event bus and engine events.
    Events,
    /// Individual plugin code.
    Plugin,
}

impl LogTarget {
    /// Parses a target string into a `LogTarget`.
    pub fn from_target_str(target: &str) -> Self {
        match target {
            log_targets::AUTH => Self::Auth,
            log_targets::STORAGE => Self::Storage,
            log_targets::RULES => Self::Rules,
            log_targets::MENU => Self::Menu,
            log_targets::I18N => Self::I18n,
            log_targets::ECS => Self::Ecs,
            log_targets::WASM => Self::Wasm,
            log_targets::PROXY => Self::Proxy,
            log_targets::ENGINE => Self::Engine,
            log_targets::REAPI => Self::Reapi,
            log_targets::WATCHER => Self::Watcher,
            log_targets::EVENTS => Self::Events,
            log_targets::PLUGIN => Self::Plugin,
            _ => Self::Core,
        }
    }

    /// Returns the lowercase name used in log lines (e.g. `"core"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => log_targets::CORE,
            Self::Auth => log_targets::AUTH,
            Self::Storage => log_targets::STORAGE,
            Self::Rules => log_targets::RULES,
            Self::Menu => log_targets::MENU,
            Self::I18n => log_targets::I18N,
            Self::Ecs => log_targets::ECS,
            Self::Wasm => log_targets::WASM,
            Self::Proxy => log_targets::PROXY,
            Self::Engine => log_targets::ENGINE,
            Self::Reapi => log_targets::REAPI,
            Self::Watcher => log_targets::WATCHER,
            Self::Events => log_targets::EVENTS,
            Self::Plugin => log_targets::PLUGIN,
        }
    }
}

/// Console output mode for errors and panics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleErrorMode {
    /// Concise notification: summary in console + notice indicating full log location.
    #[default]
    Notify,
    /// Verbose multi-line stack trace directly in server console.
    Full,
    /// Suppress error output in server console completely.
    Off,
}

/// Dedicated error output configuration under `[logging.errors]` in `goldsrc.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorLogConfig {
    /// How errors are reported to the server console. Default: `Notify`.
    #[serde(default)]
    pub console_mode: ConsoleErrorMode,

    /// Whether to write errors to `logs/error_YYYY-MM-DD.log`. Default: `true`.
    #[serde(default = "default_true")]
    pub server_log: bool,

    /// Whether to write dedicated crash reports to `logs/plugins/<plugin_name>.log`. Default: `true`.
    #[serde(default = "default_true")]
    pub plugin_log: bool,
}

impl Default for ErrorLogConfig {
    fn default() -> Self {
        Self {
            console_mode: ConsoleErrorMode::Notify,
            server_log: true,
            plugin_log: true,
        }
    }
}

/// The `[logging]` section of `goldsrc.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    /// Minimum level to emit. Default: `Info`.
    #[serde(default = "default_level")]
    pub level: LogLevel,
    /// Custom format template utilizing placeholders (e.g. "[{log:date-time}][{log:level}][{log:target}] {log:message}").
    #[serde(default = "default_format")]
    pub format: String,
    /// Restrict output to these targets. Empty vec = all targets allowed.
    #[serde(default)]
    pub targets: Vec<LogTarget>,
    /// Dedicated error output and crash dump configuration.
    #[serde(default)]
    pub errors: ErrorLogConfig,
    /// Write log lines to `<logs_dir>/goldsrc.log`.
    #[serde(default = "default_true")]
    pub file_output: bool,
    /// Forward log lines to the server console via the registered callback.
    #[serde(default = "default_true")]
    pub console_output: bool,
}

fn default_level() -> LogLevel {
    LogLevel::Info
}
fn default_true() -> bool {
    true
}
pub fn default_format() -> String {
    "[{log:date-time}][{log:level}][{log:target}] {log:message}".to_string()
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_level(),
            format: default_format(),
            file_output: true,
            console_output: true,
            targets: Vec::new(), // all targets
            errors: ErrorLogConfig::default(),
        }
    }
}

// ============================================================================
// Logger singleton
// ============================================================================

type ConsoleCb = std::sync::Arc<dyn Fn(&str) + Send + Sync + 'static>;

/// Formats the current UTC/system time as `(date_str, timestamp_str)`.
/// E.g. `("2026-08-28", "2026-08-28 01:42:00")`.
fn get_current_date_and_time() -> (String, String) {
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = duration.as_secs();

    let sec = total_secs % 60;
    let min = (total_secs / 60) % 60;
    let hour = (total_secs / 3600) % 24;

    // Days since epoch
    let mut days = (total_secs / 86400) as i64;

    // Unix epoch: 1970-01-01 (Thursday)
    let mut year = 1970;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let days_in_month = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 1;
    for &dim in &days_in_month {
        if days < dim {
            break;
        }
        days -= dim;
        month += 1;
    }
    let day = days + 1;

    let date_str = format!("{:04}-{:02}-{:02}", year, month, day);
    let time_str = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hour, min, sec
    );
    (date_str, time_str)
}

struct GoldSrcLogger {
    config: LogConfig,
    logs_dir: PathBuf,
    current_date: String,
    general_writer: Option<std::io::BufWriter<std::fs::File>>,
    error_writer: Option<std::io::BufWriter<std::fs::File>>,
    /// Optional callback forwarding messages to the server console.
    console_cb: Option<ConsoleCb>,
}

impl GoldSrcLogger {
    fn new(
        config: LogConfig,
        logs_dir: Option<PathBuf>,
        backend_type: BackendType,
        console_cb: Option<ConsoleCb>,
    ) -> Self {
        let log_dir = logs_dir.unwrap_or_else(|| {
            let fw_dir = PathResolver::framework_dir(backend_type);
            fw_dir.join(crate::paths::LOGS_DIR_NAME)
        });
        let _ = fs::create_dir_all(&log_dir);

        let (date_str, _) = get_current_date_and_time();

        Self {
            config,
            logs_dir: log_dir,
            current_date: date_str,
            general_writer: None,
            error_writer: None,
            console_cb,
        }
    }

    fn should_emit(&self, level: LogLevel, target: LogTarget) -> bool {
        if level < self.config.level {
            return false;
        }
        if self.config.targets.is_empty() {
            return true;
        }
        self.config.targets.contains(&target)
    }

    fn ensure_writers(&mut self, today: &str) {
        if self.current_date != today || self.general_writer.is_none() {
            self.current_date = today.to_string();
            let _ = fs::create_dir_all(&self.logs_dir);

            // 1. General log: logs/YYYY-MM-DD.log
            let gen_path = self.logs_dir.join(format!("{}.log", today));
            self.general_writer = OpenOptions::new()
                .create(true)
                .append(true)
                .open(gen_path)
                .ok()
                .map(std::io::BufWriter::new);

            // 2. Error log: logs/error_YYYY-MM-DD.log
            let err_path = self.logs_dir.join(format!("error_{}.log", today));
            self.error_writer = OpenOptions::new()
                .create(true)
                .append(true)
                .open(err_path)
                .ok()
                .map(std::io::BufWriter::new);
        }
    }

    fn format_message(
        template: &str,
        today: &str,
        timestamp: &str,
        level_str: &str,
        target_str: &str,
        message: &str,
    ) -> String {
        template
            .replace("{log:date-time}", timestamp)
            .replace("{log:timestamp}", timestamp)
            .replace("{date-time}", timestamp)
            .replace("{time}", timestamp)
            .replace("{log:date}", today)
            .replace("{date}", today)
            .replace("{log:level}", level_str)
            .replace("{level}", level_str)
            .replace("{log:target}", target_str)
            .replace("{log:source}", target_str)
            .replace("{target}", target_str)
            .replace("{source}", target_str)
            .replace("{log:message}", message)
            .replace("{log:msg}", message)
            .replace("{message}", message)
            .replace("{msg}", message)
    }

    fn emit(&mut self, level: LogLevel, target: LogTarget, message: &str) -> Option<String> {
        if !self.should_emit(level, target) {
            return None;
        }

        let (today, timestamp) = get_current_date_and_time();

        let mut plain_line = Self::format_message(
            &self.config.format,
            &today,
            &timestamp,
            level.as_str(),
            target.as_str(),
            message,
        );
        if !plain_line.ends_with('\n') {
            plain_line.push('\n');
        }

        // File output with daily rotation and dedicated error stream (buffered)
        if self.config.file_output {
            self.ensure_writers(&today);

            if let Some(ref mut writer) = self.general_writer {
                let _ = writer.write_all(plain_line.as_bytes());
            }

            // Write to dedicated error file if level is Error and server_log is enabled
            if level == LogLevel::Error
                && self.config.errors.server_log
                && let Some(ref mut err_writer) = self.error_writer
            {
                let _ = err_writer.write_all(plain_line.as_bytes());
                let _ = err_writer.flush();
            }

            // Dedicated per-plugin crash log if enabled
            if level == LogLevel::Error && self.config.errors.plugin_log {
                let plugin_name = if let Some(rest) = message.strip_prefix("[CRASH:") {
                    rest.split(']').next()
                } else if target == LogTarget::Plugin {
                    if message.starts_with('[')
                        && let Some(end) = message.find(']')
                    {
                        Some(&message[1..end])
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(name) = plugin_name {
                    let plugins_log_dir = self.logs_dir.join("plugins");
                    let _ = fs::create_dir_all(&plugins_log_dir);
                    let plugin_log_path = plugins_log_dir.join(format!("{name}.log"));
                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(plugin_log_path)
                    {
                        let _ = file.write_all(plain_line.as_bytes());
                        let _ = file.flush();
                    }
                }
            }
        }

        // Return formatted console string to be dispatched OUTSIDE of the logger lock
        if self.config.console_output && self.console_cb.is_some() {
            if level == LogLevel::Error {
                match self.config.errors.console_mode {
                    ConsoleErrorMode::Off => return None,
                    ConsoleErrorMode::Notify => {
                        let (headline, subline) = if let Some(rest) =
                            message.strip_prefix("[CRASH:")
                        {
                            let name = rest.split(']').next().unwrap_or("unknown");
                            let cause = message
                                .lines()
                                .find(|l| l.contains("Root Cause:"))
                                .unwrap_or("trapped");
                            let loc = message
                                .lines()
                                .find(|l| l.contains("Primary Location:"))
                                .unwrap_or("");
                            (
                                format!(
                                    "\x1b[31m[ERROR]\x1b[0m\x1b[35m[plugin]\x1b[0m Plugin '{name}' {cause} {loc}"
                                ),
                                format!(
                                    "\x1b[31m[ERROR]\x1b[0m\x1b[35m[plugin]\x1b[0m Full crash backtrace written to: logs/error_{today}.log & logs/plugins/{name}.log"
                                ),
                            )
                        } else {
                            let first_line = message.lines().next().unwrap_or(message);
                            (
                                format!(
                                    "\x1b[31m[ERROR]\x1b[0m\x1b[35m[{}]\x1b[0m {first_line}",
                                    target.as_str()
                                ),
                                format!(
                                    "\x1b[31m[ERROR]\x1b[0m Full diagnostic details written to: logs/error_{today}.log"
                                ),
                            )
                        };
                        return Some(format!("{headline}\n{subline}\n"));
                    }
                    ConsoleErrorMode::Full => {
                        // proceed to normal full console output
                    }
                }
            }

            let level_color = match level {
                LogLevel::Trace => "\x1b[36m",
                LogLevel::Debug => "\x1b[90m",
                LogLevel::Info => "\x1b[32m",
                LogLevel::Warn => "\x1b[33m",
                LogLevel::Error => "\x1b[31m",
            };
            let target_color = "\x1b[35m"; // Magenta for target
            let reset = "\x1b[0m";

            let colored_level = format!("{level_color}{}{reset}", level.as_str());
            let colored_target = format!("{target_color}{}{reset}", target.as_str());

            let mut console_line = Self::format_message(
                &self.config.format,
                &today,
                &timestamp,
                &colored_level,
                &colored_target,
                message,
            );
            if !console_line.ends_with('\n') {
                console_line.push('\n');
            }
            Some(console_line)
        } else {
            None
        }
    }
}

struct LoggerImpl {
    inner: Mutex<GoldSrcLogger>,
}

impl log::Log for LoggerImpl {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let level = LogLevel::from_log_level(metadata.level());
        // Lock-free fast-path: if message level is lower than configured minimum, drop immediately
        if (level.as_u8()) < ATOMIC_MAX_LEVEL.load(Ordering::Relaxed) {
            return false;
        }

        let target = LogTarget::from_target_str(metadata.target());
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        guard.should_emit(level, target)
    }

    fn log(&self, record: &log::Record) {
        let level = LogLevel::from_log_level(record.level());
        // Lock-free fast-path: never format args or lock mutex if level is filtered out
        if (level.as_u8()) < ATOMIC_MAX_LEVEL.load(Ordering::Relaxed) {
            return;
        }

        let target = LogTarget::from_target_str(record.target());

        let (console_line, console_cb) = {
            let mut guard = match self.inner.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            if !guard.should_emit(level, target) {
                return;
            }
            let message = record.args().to_string();
            let console_line = guard.emit(level, target, &message);
            let cb = guard.console_cb.clone();
            (console_line, cb)
        };

        // If console output is active, dispatch OUTSIDE of the logger lock to prevent deadlocks
        if let Some(line) = console_line
            && let Some(cb) = console_cb
        {
            cb(&line);
        }
    }

    fn flush(&self) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        if let Some(ref mut writer) = guard.general_writer {
            let _ = writer.flush();
        }
        if let Some(ref mut writer) = guard.error_writer {
            let _ = writer.flush();
        }
    }
}

static LOGGER_INSTANCE: std::sync::OnceLock<LoggerImpl> = std::sync::OnceLock::new();

/// Initialise the global logger. Must be called once at server startup,
/// after the engine has provided function pointers.
///
/// `console_cb` is an optional closure that forwards formatted log lines
/// to the server console via `pfnServerPrint`.
pub fn init<F>(config: LogConfig, backend_type: BackendType, console_cb: Option<F>)
where
    F: Fn(&str) + Send + Sync + 'static,
{
    init_with_dir(config, None, backend_type, console_cb);
}

/// Initialise the global logger with a custom log directory path.
pub fn init_with_dir<F>(
    config: LogConfig,
    logs_dir: Option<PathBuf>,
    backend_type: BackendType,
    console_cb: Option<F>,
) where
    F: Fn(&str) + Send + Sync + 'static,
{
    let cb: Option<ConsoleCb> = console_cb.map(|f| std::sync::Arc::new(f) as ConsoleCb);
    let level_filter = config.level.to_level_filter();
    ATOMIC_MAX_LEVEL.store(config.level.as_u8(), Ordering::Relaxed);

    if let Ok(_logger) = LOGGER_INSTANCE.set(LoggerImpl {
        inner: Mutex::new(GoldSrcLogger::new(config, logs_dir, backend_type, cb)),
    }) {
        let _ = log::set_logger(LOGGER_INSTANCE.get().unwrap());
        log::set_max_level(level_filter);
    }
}

/// Flushes any pending log records to disk.
pub fn flush() {
    if let Some(logger) = LOGGER_INSTANCE.get() {
        use log::Log;
        logger.flush();
    }
}

/// Returns the path where today's log file is written (for display / config).
pub fn log_file_path(backend: BackendType) -> PathBuf {
    let (today, _) = get_current_date_and_time();
    PathResolver::framework_dir(backend)
        .join(crate::paths::LOGS_DIR_NAME)
        .join(format!("{}.log", today))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_log_config() {
        let cfg = LogConfig::default();
        assert_eq!(cfg.level, LogLevel::Info);
        assert_eq!(
            cfg.format,
            "[{log:date-time}][{log:level}][{log:target}] {log:message}"
        );
        assert!(cfg.file_output);
        assert!(cfg.console_output);
    }

    #[test]
    fn test_format_message_interpolation() {
        let template = "{log:date-time} [{log:level}] ({log:target}) -> {log:message}";
        let formatted = GoldSrcLogger::format_message(
            template,
            "2026-09-05",
            "2026-09-05 14:30:00",
            "INFO",
            "core",
            "Server initialized",
        );
        assert_eq!(
            formatted,
            "2026-09-05 14:30:00 [INFO] (core) -> Server initialized"
        );
    }

    #[test]
    fn test_format_message_fallback_aliases() {
        let template = "[{time}][{level}][{source}] {msg}";
        let formatted = GoldSrcLogger::format_message(
            template,
            "2026-09-05",
            "2026-09-05 14:30:00",
            "WARN",
            "wasm",
            "Epoch deadline exceeded",
        );
        assert_eq!(
            formatted,
            "[2026-09-05 14:30:00][WARN][wasm] Epoch deadline exceeded"
        );
    }

    #[test]
    fn test_error_log_config_defaults() {
        let cfg = ErrorLogConfig::default();
        assert_eq!(cfg.console_mode, ConsoleErrorMode::Notify);
        assert!(cfg.server_log);
        assert!(cfg.plugin_log);
    }

    #[test]
    fn test_emit_error_notify_console_mode() {
        let cfg = LogConfig {
            level: LogLevel::Info,
            errors: ErrorLogConfig {
                console_mode: ConsoleErrorMode::Notify,
                server_log: true,
                plugin_log: true,
            },
            file_output: false,
            console_output: true,
            ..Default::default()
        };
        let mut logger = GoldSrcLogger::new(
            cfg,
            None,
            BackendType::Metamod,
            Some(std::sync::Arc::new(|_| {})),
        );
        let crash_msg = "[CRASH:test_plugin]\nRoot Cause: UnreachableCodeReached\nPrimary Location: src/lib.rs:42:5\nDemangled Backtrace:\n  frame 0";
        let console_res = logger.emit(LogLevel::Error, LogTarget::Plugin, crash_msg);
        assert!(console_res.is_some());
        let line = console_res.unwrap();
        assert!(line.contains("Plugin 'test_plugin'"));
        assert!(line.contains("Root Cause: UnreachableCodeReached"));
        assert!(line.contains("Primary Location: src/lib.rs:42:5"));
        assert!(line.contains("Full crash backtrace written to:"));
        // Ensure it does NOT contain the 30-line backtrace frames in notify mode
        assert!(!line.contains("Demangled Backtrace"));
    }
}
