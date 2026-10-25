pub use goldsrc_api::consts::{
    ADDONS_DIR_NAME, BUNDLES_DIR_NAME, BackendType, CONFIGS_DIR_NAME, DATA_DIR_NAME, DB_DIR_NAME,
    DEFAULT_CONFIG_FILE_NAME, DEFAULT_DB_FILE_NAME, DEFAULT_MOD_DIR, FRAMEWORK_NAME,
    HOSTS_DIR_NAME, LANG_DIR_NAME, LIB_DIR_NAME, LOGS_DIR_NAME, PLUGINS_DIR_NAME, WASM_EXT,
};
use std::path::{Path, PathBuf};

/// Centralized helper for resolving directory paths across HLDS environments.
pub struct PathResolver;

impl PathResolver {
    /// Gets the base directory for the framework depending on the backend.
    /// E.g. `cstrike/addons/goldsrc` for Metamod, `cstrike/goldsrc` for Standalone.
    pub fn framework_dir(backend: BackendType) -> PathBuf {
        let mut path = PathBuf::from(DEFAULT_MOD_DIR);
        if backend == BackendType::Metamod {
            path.push(ADDONS_DIR_NAME);
        }
        path.push(FRAMEWORK_NAME);
        path
    }

    /// Returns possible plugin directory paths in order of preference.
    pub fn plugin_dirs(backend: BackendType) -> Vec<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let mut dirs = Vec::new();

        let rel_path = Self::framework_dir(backend).join(PLUGINS_DIR_NAME);
        if let Some(ref base) = exe_dir {
            dirs.push(base.join(&rel_path));
            // Alternative without DEFAULT_MOD_DIR prefix
            let mut alt_rel = PathBuf::new();
            if backend == BackendType::Metamod {
                alt_rel.push(ADDONS_DIR_NAME);
            }
            alt_rel.push(FRAMEWORK_NAME);
            alt_rel.push(PLUGINS_DIR_NAME);
            dirs.push(base.join(&alt_rel));
        }

        dirs.push(rel_path);
        dirs
    }

    /// Returns the first existing plugin directory, or the primary default path.
    pub fn existing_plugin_dir(backend: BackendType) -> PathBuf {
        for dir in Self::plugin_dirs(backend) {
            if dir.exists() {
                return dir;
            }
        }
        Self::framework_dir(backend).join(PLUGINS_DIR_NAME)
    }

    /// Returns possible backend shared library directory paths (`lib/`) in order of preference.
    pub fn lib_dirs(backend: BackendType) -> Vec<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let mut dirs = Vec::new();

        let rel_path = Self::framework_dir(backend).join(LIB_DIR_NAME);
        if let Some(ref base) = exe_dir {
            dirs.push(base.join(&rel_path));
            let mut alt_rel = PathBuf::new();
            if backend == BackendType::Metamod {
                alt_rel.push(ADDONS_DIR_NAME);
            }
            alt_rel.push(FRAMEWORK_NAME);
            alt_rel.push(LIB_DIR_NAME);
            dirs.push(base.join(&alt_rel));
        }

        dirs.push(rel_path);
        dirs
    }

    /// Returns the first existing shared library directory (`lib/`), or the primary default path.
    pub fn existing_lib_dir(backend: BackendType) -> PathBuf {
        for dir in Self::lib_dirs(backend) {
            if dir.exists() {
                return dir;
            }
        }
        Self::framework_dir(backend).join(LIB_DIR_NAME)
    }

    /// Returns possible bundle directory paths in order of preference.
    pub fn bundle_dirs(backend: BackendType) -> Vec<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let mut dirs = Vec::new();

        let rel_path = Self::framework_dir(backend).join(BUNDLES_DIR_NAME);
        if let Some(ref base) = exe_dir {
            dirs.push(base.join(&rel_path));
        }
        dirs.push(rel_path);
        dirs
    }

    /// Returns the first existing bundle directory, or the primary default path.
    pub fn existing_bundle_dir(backend: BackendType) -> PathBuf {
        for dir in Self::bundle_dirs(backend) {
            if dir.exists() {
                return dir;
            }
        }
        Self::framework_dir(backend).join(BUNDLES_DIR_NAME)
    }

    /// Returns possible config directory paths in order of preference.
    pub fn config_dirs(backend: BackendType) -> Vec<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let mut dirs = Vec::new();

        let rel_path = Self::framework_dir(backend).join(CONFIGS_DIR_NAME);
        if let Some(ref base) = exe_dir {
            dirs.push(base.join(&rel_path));
            // Alternative without DEFAULT_MOD_DIR prefix
            let mut alt_rel = PathBuf::new();
            if backend == BackendType::Metamod {
                alt_rel.push(ADDONS_DIR_NAME);
            }
            alt_rel.push(FRAMEWORK_NAME);
            alt_rel.push(CONFIGS_DIR_NAME);
            dirs.push(base.join(&alt_rel));
        }

        dirs.push(rel_path);
        dirs
    }

    /// Returns the first existing config directory, or the primary default path.
    pub fn existing_config_dir(backend: BackendType) -> PathBuf {
        for dir in Self::config_dirs(backend) {
            if dir.exists() {
                return dir;
            }
        }
        Self::config_dirs(backend)
            .into_iter()
            .next()
            .unwrap_or_else(|| Self::framework_dir(backend).join(CONFIGS_DIR_NAME))
    }

    /// Returns possible log directory paths in order of preference.
    pub fn log_dirs(backend: BackendType) -> Vec<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let mut dirs = Vec::new();

        let rel_path = Self::framework_dir(backend).join(LOGS_DIR_NAME);
        if let Some(ref base) = exe_dir {
            dirs.push(base.join(&rel_path));
            // Alternative without DEFAULT_MOD_DIR prefix
            let mut alt_rel = PathBuf::new();
            if backend == BackendType::Metamod {
                alt_rel.push(ADDONS_DIR_NAME);
            }
            alt_rel.push(FRAMEWORK_NAME);
            alt_rel.push(LOGS_DIR_NAME);
            dirs.push(base.join(&alt_rel));
        }

        dirs.push(rel_path);
        dirs
    }

    /// Returns the first existing log directory, or the primary default path.
    pub fn existing_log_dir(backend: BackendType) -> PathBuf {
        for dir in Self::log_dirs(backend) {
            if dir.exists() {
                return dir;
            }
        }
        Self::log_dirs(backend)
            .into_iter()
            .next()
            .unwrap_or_else(|| Self::framework_dir(backend).join(LOGS_DIR_NAME))
    }

    /// Returns possible data directory paths in order of preference.
    pub fn data_dirs(backend: BackendType) -> Vec<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let mut dirs = Vec::new();

        let rel_path = Self::framework_dir(backend).join(DATA_DIR_NAME);
        if let Some(ref base) = exe_dir {
            // 1. Primary: <exe_dir>/cstrike/addons/goldsrc/data (for Metamod) or <exe_dir>/cstrike/goldsrc/data (for Standalone)
            dirs.push(base.join(&rel_path));
        }

        // 2. Relative to working directory: cstrike/addons/goldsrc/data
        dirs.push(rel_path);

        if let Some(ref base) = exe_dir {
            // 3. Fallback: <exe_dir>/addons/goldsrc/data (without mod dir prefix)
            let mut alt_rel = PathBuf::new();
            if backend == BackendType::Metamod {
                alt_rel.push(ADDONS_DIR_NAME);
            }
            alt_rel.push(FRAMEWORK_NAME);
            alt_rel.push(DATA_DIR_NAME);
            dirs.push(base.join(&alt_rel));
        }

        dirs
    }

    /// Returns the first existing data directory, or the primary default path.
    pub fn existing_data_dir(backend: BackendType) -> PathBuf {
        for dir in Self::data_dirs(backend) {
            if dir.exists() {
                return dir;
            }
        }
        Self::data_dirs(backend)
            .into_iter()
            .next()
            .unwrap_or_else(|| Self::framework_dir(backend).join(DATA_DIR_NAME))
    }

    /// Returns the primary localization directory (`data/lang`).
    pub fn lang_dir(backend: BackendType) -> PathBuf {
        Self::existing_data_dir(backend).join(LANG_DIR_NAME)
    }

    /// Returns the primary SQLite database path (`data/db/goldsrc.db`).
    pub fn db_path(backend: BackendType) -> PathBuf {
        Self::existing_data_dir(backend)
            .join(DB_DIR_NAME)
            .join(DEFAULT_DB_FILE_NAME)
    }

    /// Returns the path to goldsrc.toml.
    pub fn main_config_path(backend: BackendType) -> PathBuf {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));

        let rel_path = Self::framework_dir(backend).join(DEFAULT_CONFIG_FILE_NAME);
        if let Some(ref base) = exe_dir {
            let path = base.join(&rel_path);
            if path.exists() {
                return path;
            }
        }

        rel_path
    }

    /// Normalizes a path to a consistent, human-readable string with forward
    /// slashes (`/`) on all platforms.
    ///
    /// - Resolves `.` and `..` components lexically (without touching the
    ///   filesystem), so the path does not need to exist.
    /// - Converts all backslashes to forward slashes (important on Windows).
    /// - Does **not** make the path absolute; relative paths stay relative.
    ///
    /// # Example
    /// ```ignore
    /// let p = PathBuf::from(r"cstrike\addons\goldsrc\.\plugins");
    /// assert_eq!(PathResolver::normalize(&p), "cstrike/addons/goldsrc/plugins");
    /// ```
    pub fn normalize(path: &Path) -> String {
        // Pre-convert all backslashes to forward slashes so that POSIX/Linux
        // Path::components() properly parses Windows path segments.
        let string_repr = path.to_string_lossy().replace('\\', "/");
        let clean_path = Path::new(&string_repr);

        // Resolve . and .. without hitting the filesystem.
        let mut components: Vec<std::path::Component> = Vec::new();
        for comp in clean_path.components() {
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

        result
    }

    /// Normalizes a path into a canonicalized `PathBuf` with forward slashes and resolved dot components.
    pub fn normalize_path(path: &Path) -> PathBuf {
        PathBuf::from(Self::normalize(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_forward_slashes_unchanged() {
        let p = PathBuf::from("cstrike/addons/goldsrc/plugins");
        assert_eq!(
            PathResolver::normalize(&p),
            "cstrike/addons/goldsrc/plugins"
        );
    }

    #[test]
    fn normalize_backslashes_converted() {
        // On Windows PathBuf may store backslashes; we always want forward slashes.
        let p = PathBuf::from(r"cstrike\addons\goldsrc\plugins");
        assert_eq!(
            PathResolver::normalize(&p),
            "cstrike/addons/goldsrc/plugins"
        );
    }

    #[test]
    fn normalize_strips_cur_dir() {
        let p = PathBuf::from("cstrike/./addons/./goldsrc");
        assert_eq!(PathResolver::normalize(&p), "cstrike/addons/goldsrc");
    }

    #[test]
    fn normalize_resolves_parent_dir() {
        let p = PathBuf::from("cstrike/addons/../goldsrc");
        assert_eq!(PathResolver::normalize(&p), "cstrike/goldsrc");
    }

    #[test]
    fn normalize_mixed_separators() {
        let p = PathBuf::from(r"cstrike\addons/goldsrc\..\goldsrc\plugins");
        assert_eq!(
            PathResolver::normalize(&p),
            "cstrike/addons/goldsrc/plugins"
        );
    }

    #[test]
    fn normalize_windows_absolute_path() {
        let p = PathBuf::from(r"C:\Users\Administrator\Desktop\server\goldsrc.toml");
        assert_eq!(
            PathResolver::normalize(&p),
            "C:/Users/Administrator/Desktop/server/goldsrc.toml"
        );
    }
}
