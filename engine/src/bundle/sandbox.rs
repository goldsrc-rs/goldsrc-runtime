//! Strict filesystem sandboxing and path traversal prevention for WASM bundles.

use crate::paths::{BackendType, PathResolver};
use std::fmt;
use std::path::{Path, PathBuf};

/// Errors returned by the filesystem sandboxing subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// Attempted path traversal via `..` or illegal path tokens.
    PathTraversalAttempt(String),
    /// Requested path resolves outside the permitted server root.
    OutsideServerRoot(String),
    /// Write access denied because the target path lies outside the bundle's jailed data directory.
    WriteRestricted {
        requested: String,
        allowed_root: String,
    },
    /// Path contains illegal null characters or is malformed.
    InvalidPath(String),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathTraversalAttempt(p) => {
                write!(f, "sandbox blocked path traversal attempt in '{p}'")
            }
            Self::OutsideServerRoot(p) => {
                write!(f, "sandbox blocked access outside server root for '{p}'")
            }
            Self::WriteRestricted {
                requested,
                allowed_root,
            } => {
                write!(
                    f,
                    "write access to '{requested}' denied; writes are strictly jailed to '{allowed_root}'"
                )
            }
            Self::InvalidPath(p) => write!(f, "invalid filesystem path '{p}'"),
        }
    }
}

impl std::error::Error for SandboxError {}

/// Sandboxing policy governing filesystem operations for a specific WASM bundle or plugin.
#[derive(Debug, Clone)]
pub struct BundleFsSandbox {
    bundle_name: String,
    server_root: PathBuf,
    data_dir: PathBuf,
    config_dir: PathBuf,
}

impl BundleFsSandbox {
    /// Creates a new filesystem sandbox for a bundle under the designated backend.
    pub fn new(
        bundle_name: impl Into<String>,
        server_root: impl Into<PathBuf>,
        backend: BackendType,
    ) -> Self {
        let b_name = bundle_name.into();
        let s_root = server_root.into();

        let fw_rel = match backend {
            BackendType::Metamod => PathBuf::from(goldsrc_api::consts::ADDONS_DIR_NAME)
                .join(goldsrc_api::consts::FRAMEWORK_NAME),
            BackendType::Standalone => PathBuf::from(goldsrc_api::consts::FRAMEWORK_NAME),
        };

        let base = s_root.clone();

        let data_dir = base
            .join(&fw_rel)
            .join(goldsrc_api::consts::DATA_DIR_NAME)
            .join(&b_name);

        let config_dir = base
            .join(&fw_rel)
            .join(goldsrc_api::consts::CONFIGS_DIR_NAME)
            .join(&b_name);

        Self {
            bundle_name: b_name,
            server_root: s_root,
            data_dir,
            config_dir,
        }
    }

    /// Bundle name associated with this sandbox.
    #[inline]
    pub fn bundle_name(&self) -> &str {
        &self.bundle_name
    }

    /// Path to the bundle's exclusive writable data directory.
    #[inline]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Path to the bundle's read-only configuration directory.
    #[inline]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Validates and resolves a path intended for reading.
    ///
    /// Reads are permitted across legitimate server directories (config, data, lang)
    /// but strictly forbidden from escaping `server_root` or using `..` traversal.
    pub fn resolve_read_path(&self, requested: &Path) -> Result<PathBuf, SandboxError> {
        let requested_str = requested.to_string_lossy();
        if requested_str.contains('\0') {
            return Err(SandboxError::InvalidPath(requested_str.to_string()));
        }

        // Check for literal traversal components
        for comp in requested.components() {
            if matches!(comp, std::path::Component::ParentDir) {
                return Err(SandboxError::PathTraversalAttempt(
                    requested_str.to_string(),
                ));
            }
        }

        let full_path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.server_root.join(requested)
        };

        let norm_full = PathResolver::normalize(&full_path);
        let norm_root = PathResolver::normalize(&self.server_root);

        if !norm_full.starts_with(&norm_root) {
            return Err(SandboxError::OutsideServerRoot(requested_str.to_string()));
        }

        Ok(full_path)
    }

    /// Validates and resolves a path intended for writing.
    ///
    /// Writes are strictly and exclusively jailed to the bundle's private `data/<bundle_name>/` directory.
    /// Any attempt to write outside this jail (e.g. to binaries, other bundles, server root) is rejected.
    pub fn resolve_write_path(&self, requested: &Path) -> Result<PathBuf, SandboxError> {
        let requested_str = requested.to_string_lossy();
        if requested_str.contains('\0') {
            return Err(SandboxError::InvalidPath(requested_str.to_string()));
        }

        for comp in requested.components() {
            if matches!(comp, std::path::Component::ParentDir) {
                return Err(SandboxError::PathTraversalAttempt(
                    requested_str.to_string(),
                ));
            }
        }

        let full_path = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            let requested_norm = PathResolver::normalize(requested);
            let s_root_norm = PathResolver::normalize(&self.server_root);
            if !s_root_norm.is_empty() && requested_norm.starts_with(&s_root_norm) {
                self.server_root.join(
                    requested
                        .strip_prefix(&self.server_root)
                        .unwrap_or(requested),
                )
            } else {
                self.data_dir.join(requested)
            }
        };

        let norm_full = PathResolver::normalize(&full_path);
        let norm_data = PathResolver::normalize(&self.data_dir);

        if !norm_full.starts_with(&norm_data) {
            return Err(SandboxError::WriteRestricted {
                requested: requested_str.to_string(),
                allowed_root: norm_data,
            });
        }

        Ok(full_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_allows_data_writes() {
        let sandbox =
            BundleFsSandbox::new("vip_system", PathBuf::from("cstrike"), BackendType::Metamod);

        let res = sandbox.resolve_write_path(Path::new("players.json"));
        assert!(res.is_ok());
        let norm = PathResolver::normalize(&res.unwrap());
        assert_eq!(norm, "cstrike/addons/goldsrc/data/vip_system/players.json");
    }

    #[test]
    fn test_sandbox_blocks_write_outside_data_jail() {
        let sandbox =
            BundleFsSandbox::new("vip_system", PathBuf::from("cstrike"), BackendType::Metamod);

        // Attempting to write to server binary or configs
        let bad_path = Path::new("cstrike/addons/goldsrc/lib/evil.dll");
        let err = sandbox.resolve_write_path(bad_path).unwrap_err();
        assert!(matches!(err, SandboxError::WriteRestricted { .. }));
    }

    #[test]
    fn test_sandbox_blocks_path_traversal() {
        let sandbox =
            BundleFsSandbox::new("vip_system", PathBuf::from("cstrike"), BackendType::Metamod);

        let traversal = Path::new("../../../etc/passwd");
        let err = sandbox.resolve_read_path(traversal).unwrap_err();
        assert!(matches!(err, SandboxError::PathTraversalAttempt(_)));

        let err2 = sandbox.resolve_write_path(traversal).unwrap_err();
        assert!(matches!(err2, SandboxError::PathTraversalAttempt(_)));
    }
}
