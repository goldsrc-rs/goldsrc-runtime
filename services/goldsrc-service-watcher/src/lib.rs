//! Unified filesystem watcher service and hot-reload debouncer for GoldSrc.rs.
//!
//! Provides declarative specification of filesystem watch targets ([`WatchTarget`]),
//! granular zero-allocation filtering ([`WatcherFilter`]), per-watcher debouncing,
//! dynamic pause/resume, and telemetry status inspection for admin tooling and CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use goldsrc_api::consts::log_targets;
use notify::Watcher;

/// Standard default debounce delay between reloads of the same file (500ms).
pub const DEFAULT_RELOAD_DEBOUNCE: Duration = Duration::from_millis(500);

/// Normalizes a path into a canonicalized `PathBuf` with forward slashes and resolved dot components.
pub fn normalize_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut components = Vec::new();
    for comp in Path::new(&s).components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(components.last(), Some(std::path::Component::Normal(_))) {
                    components.pop();
                }
            }
            other => components.push(other),
        }
    }

    let mut result = String::new();
    for comp in components {
        match comp {
            std::path::Component::Prefix(prefix) => {
                result.push_str(&prefix.as_os_str().to_string_lossy());
            }
            std::path::Component::RootDir => {
                if !result.ends_with('/') {
                    result.push('/');
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !result.is_empty() && !result.ends_with('/') {
                    result.push('/');
                }
                result.push_str("..");
            }
            std::path::Component::Normal(c) => {
                if !result.is_empty() && !result.ends_with('/') {
                    result.push('/');
                }
                result.push_str(&c.to_string_lossy());
            }
        }
    }

    PathBuf::from(result)
}

/// Granular filter applied to filesystem modification events in watched targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatcherFilter {
    /// Matches all files (`*`).
    Any,
    /// Fast zero-allocation matching by file extension (e.g. `"wasm"`, `"toml"`).
    Extension(&'static str),
    /// Fast zero-allocation matching by file stem without extension (e.g. `"plugins"` for `plugins.toml`).
    Stem(&'static str),
    /// Fast zero-allocation matching by exact filename (e.g. `"plugins.toml"`).
    ExactName(&'static str),
    /// Full pattern / substring glob matching.
    Pattern(String),
}

impl WatcherFilter {
    /// Evaluates whether `path` satisfies the filter.
    pub fn matches(&self, path: &Path) -> bool {
        match self {
            Self::Any => true,
            Self::Extension(ext) => path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(ext)),
            Self::Stem(stem) => path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(stem)),
            Self::ExactName(name) => path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case(name)),
            Self::Pattern(pat) => {
                let s = path.to_string_lossy();
                s.contains(pat)
            }
        }
    }

    /// Human-readable label for inspection and CLI.
    pub fn display_label(&self) -> String {
        match self {
            Self::Any => "*".to_string(),
            Self::Extension(ext) => format!("*.{ext}"),
            Self::Stem(stem) => format!("{stem}.*"),
            Self::ExactName(name) => (*name).to_string(),
            Self::Pattern(pat) => pat.clone(),
        }
    }
}

/// Target of a filesystem watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchTarget {
    /// Watch a specific standalone file directly.
    File(PathBuf),
    /// Watch a directory tree with recursive flag and file filter.
    Directory {
        path: PathBuf,
        recursive: bool,
        filter: WatcherFilter,
    },
}

impl WatchTarget {
    /// Returns the root path monitored by this target.
    pub fn root_path(&self) -> &Path {
        match self {
            Self::File(p) => p.as_path(),
            Self::Directory { path, .. } => path.as_path(),
        }
    }

    /// Evaluates if the event path matches this target's criteria.
    pub fn matches_event(&self, event_path: &Path) -> bool {
        let norm_event = normalize_path(event_path);
        match self {
            Self::File(expected_file) => {
                let norm_expected = normalize_path(expected_file);
                norm_event == norm_expected
                    || event_path == expected_file
                    || (cfg!(windows)
                        && norm_event
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&norm_expected.to_string_lossy()))
                    || event_path
                        .canonicalize()
                        .ok()
                        .zip(expected_file.canonicalize().ok())
                        .is_some_and(|(a, b)| {
                            if cfg!(windows) {
                                a.to_string_lossy()
                                    .eq_ignore_ascii_case(&b.to_string_lossy())
                            } else {
                                a == b
                            }
                        })
            }
            Self::Directory {
                path: base_dir,
                filter,
                ..
            } => {
                let norm_base = normalize_path(base_dir);
                let matched_base = if cfg!(windows) {
                    norm_event.starts_with(&norm_base)
                        || event_path.starts_with(base_dir)
                        || norm_event
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .starts_with(&norm_base.to_string_lossy().to_ascii_lowercase())
                } else {
                    norm_event.starts_with(&norm_base) || event_path.starts_with(base_dir)
                };

                if !matched_base {
                    if let (Ok(can_event), Ok(can_base)) =
                        (event_path.canonicalize(), base_dir.canonicalize())
                    {
                        let can_matched = if cfg!(windows) {
                            can_event
                                .to_string_lossy()
                                .to_ascii_lowercase()
                                .starts_with(&can_base.to_string_lossy().to_ascii_lowercase())
                        } else {
                            can_event.starts_with(&can_base)
                        };
                        if !can_matched {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                filter.matches(&norm_event)
            }
        }
    }
}

/// Declarative specification for registering a filesystem watcher.
#[derive(Debug, Clone)]
pub struct WatcherSpec {
    /// Namespaced identifier in `<source>:<name>` format (e.g. `"core:plugins"`, `"i18n:dicts"`).
    pub id: String,
    /// Watch target (File or Directory).
    pub target: WatchTarget,
    /// Minimum time between reloads of the same file.
    pub debounce: Duration,
}

impl WatcherSpec {
    /// Creates a directory watcher specification with standard 500ms debounce.
    pub fn directory<P: Into<PathBuf>>(
        id: impl Into<String>,
        path: P,
        filter: WatcherFilter,
        recursive: bool,
    ) -> Self {
        let p = path.into();
        Self {
            id: id.into(),
            target: WatchTarget::Directory {
                path: normalize_path(&p),
                recursive,
                filter,
            },
            debounce: DEFAULT_RELOAD_DEBOUNCE,
        }
    }

    /// Creates a file watcher specification with standard 500ms debounce.
    pub fn file<P: Into<PathBuf>>(id: impl Into<String>, path: P) -> Self {
        let p = path.into();
        Self {
            id: id.into(),
            target: WatchTarget::File(normalize_path(&p)),
            debounce: DEFAULT_RELOAD_DEBOUNCE,
        }
    }

    /// Sets custom debounce duration.
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }
}

/// An incoming filesystem event emitted by the watcher service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatcherEvent {
    /// Registered ID of the watcher that triggered this event.
    pub watcher_id: String,
    /// Path of the modified / created file.
    pub path: PathBuf,
}

use stitch_rs::flow::FlowControl;

/// 64-byte aligned execution context for filesystem watch events under SMA.
#[repr(C, align(64))]
pub struct WatcherContext<'a> {
    pub watcher_id: &'a str,
    pub path: &'a Path,
    pub is_suppressed: bool,
}

impl<'a> stitch_rs::Blackboard for WatcherContext<'a> {}

impl<'a> WatcherContext<'a> {
    pub fn new(watcher_id: &'a str, path: &'a Path) -> Self {
        Self {
            watcher_id,
            path,
            is_suppressed: false,
        }
    }

    pub fn suppress(&mut self) {
        self.is_suppressed = true;
    }
}

/// The intent entering watch event processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatcherIntent<'a> {
    pub watcher_id: &'a str,
}

/// Middleware layer for intercepting, filtering, or auditing filesystem events.
pub trait WatcherLayer: Send + Sync {
    /// Descent phase: inspect event, debounce or filter.
    fn on_enter(
        &self,
        ctx: &mut WatcherContext<'_>,
        intent: &WatcherIntent<'_>,
    ) -> FlowControl<(), (), ()>;

    /// Ascent phase: audit logging, external telemetry fan-out.
    fn on_exit(
        &self,
        ctx: &mut WatcherContext<'_>,
        intent: &WatcherIntent<'_>,
        outcome: &mut Result<(), ()>,
    );
}

/// Terminal leaf node terminating watcher event U-cycle descent.
#[derive(Debug, Clone, Copy, Default)]
pub struct WatcherTerminal;

impl stitch_rs::middleware::Terminal<WatcherContext<'_>, WatcherIntent<'_>, (), ()>
    for WatcherTerminal
{
    #[inline(always)]
    fn execute(
        &mut self,
        ctx: &mut WatcherContext<'_>,
        _intent: WatcherIntent<'_>,
    ) -> Result<(), ()> {
        if ctx.is_suppressed { Err(()) } else { Ok(()) }
    }
}

/// Dynamic SMA watcher pipeline orchestrator.
#[derive(Default, Clone)]
pub struct WatcherPipeline {
    layers: Vec<std::sync::Arc<dyn WatcherLayer>>,
}

impl WatcherPipeline {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    pub fn with_layer(mut self, layer: std::sync::Arc<dyn WatcherLayer>) -> Self {
        self.layers.push(layer);
        self
    }

    #[allow(clippy::result_unit_err)]
    pub fn dispatch(
        &self,
        ctx: &mut WatcherContext<'_>,
        intent: &WatcherIntent<'_>,
    ) -> Result<(), ()> {
        let mut entered_count = 0;
        let mut halted = false;

        for layer in &self.layers {
            match layer.on_enter(ctx, intent) {
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
            layer.on_exit(ctx, intent, &mut outcome);
        }

        outcome
    }
}

/// Summary statistics of active/paused watchers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct WatcherOverview {
    pub total: usize,
    pub active: usize,
    pub paused: usize,
}

/// Public telemetry status of a registered watcher.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WatcherStatus {
    pub id: String,
    pub target_type: &'static str,
    pub path: PathBuf,
    pub filter_desc: String,
    pub debounce_ms: u64,
    pub events_fired: u64,
    #[serde(skip_serializing)]
    pub last_event: Option<Instant>,
    pub recursive: bool,
    pub is_paused: bool,
}

/// Errors that can occur during watcher registration and operation.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    #[error("Watcher IO error on '{0}': {1}")]
    Io(PathBuf, #[source] std::io::Error),

    #[error("Notify system error on '{0}': {1}")]
    Notify(PathBuf, #[source] notify::Error),

    #[error("Watcher with ID '{0}' is already registered")]
    AlreadyExists(String),
}

struct InternalWatcherEntry {
    spec: WatcherSpec,
    _handle: notify::RecommendedWatcher,
    events_fired: u64,
    last_event: Option<Instant>,
    last_reload_by_path: HashMap<PathBuf, Instant>,
    is_paused: bool,
}

/// Centralized filesystem watcher service managing system and plugin hot-reload hooks.
pub struct WatcherService {
    watchers: HashMap<String, InternalWatcherEntry>,
    event_tx: crossbeam_channel::Sender<WatcherEvent>,
    event_rx: crossbeam_channel::Receiver<WatcherEvent>,
}

impl Default for WatcherService {
    fn default() -> Self {
        Self::new()
    }
}

impl WatcherService {
    /// Creates a new uninitialized `WatcherService`.
    pub fn new() -> Self {
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        Self {
            watchers: HashMap::new(),
            event_tx,
            event_rx,
        }
    }

    /// Registers and activates a new watcher according to [`WatcherSpec`].
    pub fn register(&mut self, mut spec: WatcherSpec) -> Result<(), WatcherError> {
        if self.watchers.contains_key(&spec.id) {
            return Err(WatcherError::AlreadyExists(spec.id));
        }

        // Canonicalize target path to ensure forward-slash consistency across all platforms
        match &mut spec.target {
            WatchTarget::File(p) => {
                *p = normalize_path(p);
            }
            WatchTarget::Directory { path, .. } => {
                *path = normalize_path(path);
            }
        }

        let watch_path = match &spec.target {
            WatchTarget::File(p) => {
                if let Some(parent) = p.parent() {
                    parent.to_path_buf()
                } else {
                    p.clone()
                }
            }
            WatchTarget::Directory { path, .. } => path.clone(),
        };

        let _ = std::fs::create_dir_all(&watch_path);

        let tx = self.event_tx.clone();
        let target_clone = spec.target.clone();
        let id_clone = spec.id.clone();

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(event) => {
                    // Ignore non-mutating events (access or other)
                    match event.kind {
                        notify::EventKind::Access(_) | notify::EventKind::Other => return,
                        _ => {}
                    }

                    for p in event.paths {
                        let norm_p = normalize_path(&p);
                        if target_clone.matches_event(&norm_p) {
                            // If the path exists on disk, ensure it's not a 0-byte temporary file during atomic write/rename
                            if norm_p.is_file()
                                && let Ok(meta) = std::fs::metadata(&norm_p)
                                && meta.len() == 0
                            {
                                continue;
                            }
                            let _ = tx.send(WatcherEvent {
                                watcher_id: id_clone.clone(),
                                path: norm_p,
                            });
                        }
                    }
                }
                Err(err) => {
                    log::warn!(
                        target: log_targets::WATCHER,
                        "Filesystem watcher error for '{}': {err}",
                        id_clone
                    );
                }
            }
        })
        .map_err(|e| WatcherError::Notify(watch_path.clone(), e))?;

        let mode = match &spec.target {
            WatchTarget::File(_) => notify::RecursiveMode::NonRecursive,
            WatchTarget::Directory { recursive, .. } => {
                if *recursive {
                    notify::RecursiveMode::Recursive
                } else {
                    notify::RecursiveMode::NonRecursive
                }
            }
        };

        watcher
            .watch(&watch_path, mode)
            .map_err(|e| WatcherError::Notify(watch_path, e))?;

        self.watchers.insert(
            spec.id.clone(),
            InternalWatcherEntry {
                spec,
                _handle: watcher,
                is_paused: false,
                events_fired: 0,
                last_event: None,
                last_reload_by_path: HashMap::new(),
            },
        );

        Ok(())
    }

    /// Suspends event forwarding for watcher `id`.
    pub fn pause(&mut self, id: &str) -> bool {
        if let Some(entry) = self.watchers.get_mut(id) {
            entry.is_paused = true;
            true
        } else {
            false
        }
    }

    /// Resumes event forwarding for watcher `id`.
    pub fn resume(&mut self, id: &str) -> bool {
        if let Some(entry) = self.watchers.get_mut(id) {
            entry.is_paused = false;
            true
        } else {
            false
        }
    }

    /// Returns the number of currently registered watchers.
    pub fn watcher_count(&self) -> usize {
        self.watchers.len()
    }

    /// Returns high-level statistics of registered watchers.
    pub fn status(&self) -> WatcherOverview {
        let total = self.watchers.len();
        let paused = self.watchers.values().filter(|w| w.is_paused).count();
        WatcherOverview {
            total,
            active: total.saturating_sub(paused),
            paused,
        }
    }

    /// Drains all debounced filesystem events from the channel that occurred since last call.
    /// Drops duplicate events within the configured debounce window per watcher/path.
    pub fn drain_events(&mut self) -> Vec<WatcherEvent> {
        let mut ready = Vec::new();
        let now = Instant::now();

        while let Ok(event) = self.event_rx.try_recv() {
            let Some(entry) = self.watchers.get_mut(&event.watcher_id) else {
                continue;
            };

            if entry.is_paused {
                continue;
            }

            if let Some(last) = entry.last_reload_by_path.get(&event.path)
                && now.duration_since(*last) < entry.spec.debounce
            {
                continue;
            }

            entry.last_reload_by_path.insert(event.path.clone(), now);
            entry.events_fired += 1;
            entry.last_event = Some(now);

            ready.push(event);
        }

        ready
    }

    /// Returns telemetry status for all registered watchers, sorted by ID.
    pub fn list_watchers(&self) -> Vec<WatcherStatus> {
        let mut list: Vec<_> = self
            .watchers
            .values()
            .map(|w| {
                let (target_type, filter_desc, recursive) = match &w.spec.target {
                    WatchTarget::File(_) => ("File", "-".to_string(), false),
                    WatchTarget::Directory {
                        filter, recursive, ..
                    } => ("Directory", filter.display_label(), *recursive),
                };

                WatcherStatus {
                    id: w.spec.id.clone(),
                    target_type,
                    path: normalize_path(w.spec.target.root_path()),
                    filter_desc,
                    recursive,
                    is_paused: w.is_paused,
                    debounce_ms: w.spec.debounce.as_millis() as u64,
                    events_fired: w.events_fired,
                    last_event: w.last_event,
                }
            })
            .collect();

        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_watcher_filter_matches_correctly() {
        let wasm_filter = WatcherFilter::Extension("wasm");
        assert!(wasm_filter.matches(Path::new("cstrike/plugins/vip.wasm")));
        assert!(!wasm_filter.matches(Path::new("cstrike/plugins/vip.toml")));

        let stem_filter = WatcherFilter::Stem("plugins");
        assert!(stem_filter.matches(Path::new("cstrike/configs/plugins.toml")));
        assert!(stem_filter.matches(Path::new("cstrike/configs/plugins.json")));
        assert!(!stem_filter.matches(Path::new("cstrike/configs/other.toml")));

        let exact_filter = WatcherFilter::ExactName("plugins.toml");
        assert!(exact_filter.matches(Path::new("configs/plugins.toml")));
        assert!(!exact_filter.matches(Path::new("configs/plugins.json")));

        let pattern_filter = WatcherFilter::Pattern("lang".to_string());
        assert!(pattern_filter.matches(Path::new("data/lang/vip.toml")));
        assert!(!pattern_filter.matches(Path::new("data/sounds/beep.wav")));

        let any_filter = WatcherFilter::Any;
        assert!(any_filter.matches(Path::new("any/file.txt")));
    }

    #[test]
    fn test_watch_target_file_and_directory_evaluation() {
        let file_target = WatchTarget::File(PathBuf::from("configs/plugins.toml"));
        assert!(file_target.matches_event(Path::new("configs/plugins.toml")));
        assert!(!file_target.matches_event(Path::new("configs/other.toml")));

        let dir_target = WatchTarget::Directory {
            path: PathBuf::from("plugins"),
            recursive: true,
            filter: WatcherFilter::Extension("wasm"),
        };
        assert!(dir_target.matches_event(Path::new("plugins/vip.wasm")));
        assert!(dir_target.matches_event(Path::new("plugins/nested/sub.wasm")));
        assert!(!dir_target.matches_event(Path::new("plugins/vip.toml")));
    }

    #[test]
    fn test_watch_target_cross_platform_path_separators() {
        let file_target = WatchTarget::File(PathBuf::from(r"cstrike\configs\plugins.toml"));
        assert!(file_target.matches_event(Path::new("cstrike/configs/plugins.toml")));
        assert!(file_target.matches_event(Path::new(r"cstrike\configs\plugins.toml")));

        let dir_target = WatchTarget::Directory {
            path: PathBuf::from(r"cstrike\plugins"),
            recursive: true,
            filter: WatcherFilter::Extension("wasm"),
        };
        assert!(dir_target.matches_event(Path::new("cstrike/plugins/vip.wasm")));
        assert!(dir_target.matches_event(Path::new(r"cstrike\plugins\vip.wasm")));
        assert!(dir_target.matches_event(Path::new(r"cstrike\plugins\sub\admin.wasm")));
        assert!(dir_target.matches_event(Path::new("cstrike/plugins/sub/admin.wasm")));
    }

    #[test]
    fn test_watcher_spec_and_status_normalize_paths() {
        let dir_spec = WatcherSpec::directory(
            "test:plugins",
            r"cstrike\addons\..\plugins",
            WatcherFilter::Extension("wasm"),
            true,
        );
        assert_eq!(
            dir_spec.target.root_path().to_str().unwrap(),
            "cstrike/plugins"
        );

        let file_spec =
            WatcherSpec::file("test:config", r"cstrike\configs\..\configs\plugins.toml");
        assert_eq!(
            file_spec.target.root_path().to_str().unwrap(),
            "cstrike/configs/plugins.toml"
        );

        let temp_dir = std::env::temp_dir().join("goldsrc_test_watcher_spec");
        let target_path = temp_dir.join(r"server\cstrike\plugins");

        let mut service = WatcherService::new();
        service
            .register(WatcherSpec {
                id: "manual:raw".into(),
                target: WatchTarget::Directory {
                    path: target_path.clone(),
                    recursive: false,
                    filter: WatcherFilter::Any,
                },
                debounce: DEFAULT_RELOAD_DEBOUNCE,
            })
            .expect("registration should succeed");

        let watchers = service.list_watchers();
        assert_eq!(watchers.len(), 1);
        let expected = normalize_path(&target_path);
        assert_eq!(watchers[0].path, expected);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_watcher_context_alignment() {
        let p = Path::new("cstrike/plugins/admin.wasm");
        let ctx = WatcherContext::new("core:plugins", p);
        assert_eq!(std::mem::align_of_val(&ctx), 64);
        assert!(!ctx.is_suppressed);
    }

    struct MockWatcherSecurityLayer;

    impl WatcherLayer for MockWatcherSecurityLayer {
        fn on_enter(
            &self,
            ctx: &mut WatcherContext<'_>,
            intent: &WatcherIntent<'_>,
        ) -> FlowControl<(), (), ()> {
            if intent.watcher_id == "blocked:watcher" {
                ctx.suppress();
                return FlowControl::Halt(());
            }
            FlowControl::Proceed(())
        }

        fn on_exit(
            &self,
            _ctx: &mut WatcherContext<'_>,
            _intent: &WatcherIntent<'_>,
            _outcome: &mut Result<(), ()>,
        ) {
        }
    }

    #[test]
    fn test_watcher_pipeline_u_cycle() {
        let pipeline =
            WatcherPipeline::new().with_layer(std::sync::Arc::new(MockWatcherSecurityLayer));
        let p = Path::new("test.txt");

        let mut ctx = WatcherContext::new("blocked:watcher", p);
        let intent = WatcherIntent {
            watcher_id: "blocked:watcher",
        };

        let res = pipeline.dispatch(&mut ctx, &intent);
        assert_eq!(res, Err(()));
        assert!(ctx.is_suppressed);
    }
}
