//! WASM Component compilation, instantiation, metadata extraction, and linker setup.

use crate::bindings::{GoldsrcPlugin, goldsrc::engine::api};
use crate::error::LoadError;
use crate::manager::state::HostState;
use crate::plugin::{LoadedPlugin, PluginMetadata, PluginStatus};
use goldsrc_api::consts::log_targets;
use goldsrc_spi::engine::EnginePort as GoldsrcEngine;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wasmtime::Engine;
use wasmtime::component::{Component, Linker};

/// Computes a deterministic 128-bit FNV-1a content hash over bytes.
#[inline]
fn fnv1a_128(bytes: &[u8]) -> (u64, u64) {
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x84222325cbf29ce4;
    for &byte in bytes {
        h1 ^= byte as u64;
        h1 = h1.wrapping_mul(0x100000001b3);
        h2 = h2.wrapping_add(byte as u64);
        h2 = h2.rotate_left(7) ^ h1;
    }
    (h1, h2)
}

/// Computes a deterministic 128-bit FNV-1a content hash over bytes, returning a 32-character hex string.
#[inline]
fn compute_content_hash(bytes: &[u8]) -> String {
    let (h1, h2) = fnv1a_128(bytes);
    format!("{:016x}{:016x}", h1, h2)
}

/// Removes obsolete .cwasm and .tmp cache files for the given plugin in the cache directory.
fn clean_old_cache_files(cache_dir: &Path, plugin_name: &str, current_hash: &str) {
    let prefix = format!("{plugin_name}.");
    let current_name = format!("{plugin_name}.{current_hash}.cwasm");
    if let Ok(entries) = fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str.starts_with(&prefix)
                && (name_str.ends_with(".cwasm") || name_str.ends_with(".tmp"))
                && name_str != current_name
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Saves an AOT-precompiled component into the cache directory atomically.
fn save_aot_cache(
    engine: &Engine,
    component_bytes: &[u8],
    cache_dir: &Path,
    cache_file: &Path,
    plugin_name: &str,
    current_hash: &str,
) {
    if let Err(e) = fs::create_dir_all(cache_dir) {
        log::debug!(
            target: log_targets::WASM,
            "Could not create cache dir {:?}: {e}",
            cache_dir
        );
        return;
    }
    match engine.precompile_component(component_bytes) {
        Ok(cwasm_bytes) => {
            let tmp_file = cache_dir.join(format!("{plugin_name}.{current_hash}.tmp"));
            if fs::write(&tmp_file, &cwasm_bytes).is_ok() {
                if let Err(e) = fs::rename(&tmp_file, cache_file) {
                    log::warn!(
                        target: log_targets::WASM,
                        "Failed to rename temporary cwasm {:?} to {:?}: {e}",
                        tmp_file,
                        cache_file
                    );
                    let _ = fs::remove_file(&tmp_file);
                } else {
                    log::debug!(
                        target: log_targets::WASM,
                        "AOT compiled '{plugin_name}' cached ({:.2} KB)",
                        cwasm_bytes.len() as f64 / 1024.0
                    );
                    clean_old_cache_files(cache_dir, plugin_name, current_hash);
                }
            }
        }
        Err(err) => {
            log::warn!(
                target: log_targets::WASM,
                "Failed to precompile component for '{plugin_name}': {err}"
            );
        }
    }
}

/// Compiles and instantiates a WASM plugin component without registering or running `on_load`.
pub fn instantiate_plugin<P: AsRef<Path>>(
    engine: &Engine,
    engine_ops: &Arc<dyn GoldsrcEngine>,
    path: P,
) -> Result<LoadedPlugin, LoadError> {
    let path = path.as_ref();
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let metadata = fs::metadata(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > 32 * 1024 * 1024 {
        return Err(LoadError::Compile(format!(
            "Plugin size ({} bytes) exceeds maximum allowed size (32MB)",
            metadata.len()
        )));
    }

    let is_direct_cwasm = path.extension().and_then(|e| e.to_str()) == Some("cwasm");

    let component = if is_direct_cwasm {
        // SAFETY: Loading verified precompiled cwasm directly from disk.
        unsafe {
            Component::deserialize_file(engine, path).map_err(|e| {
                LoadError::Compile(format!(
                    "Failed to deserialize precompiled cwasm at {:?}: {e}",
                    path
                ))
            })?
        }
    } else {
        let bytes = fs::read(path).map_err(|source| LoadError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        let is_comp = bytes.len() >= 8 && &bytes[0..8] == b"\0asm\x0d\0\x01\0";
        let component_bytes = if is_comp {
            bytes
        } else {
            let mut resolve = wit_parser::Resolve::default();
            let pkg = resolve
                .push_str("goldsrc.wit", include_str!("../../wit/goldsrc.wit"))
                .map_err(|e| {
                    LoadError::Embed(format!("Failed to parse embedded goldsrc.wit: {e}"))
                })?;
            let world_id = resolve
                .select_world(&[pkg], Some("goldsrc-plugin"))
                .map_err(|e| {
                    LoadError::Embed(format!("Failed to select goldsrc-plugin world: {e}"))
                })?;

            let mut wasm_bytes = bytes.to_vec();
            wit_component::embed_component_metadata(
                &mut wasm_bytes,
                &resolve,
                world_id,
                wit_component::StringEncoding::UTF8,
            )
            .map_err(|e| LoadError::Embed(e.to_string()))?;

            let mut base_encoder = wit_component::ComponentEncoder::default();
            let encoder = base_encoder.validate(true);
            let encoder = encoder
                .module(&wasm_bytes)
                .map_err(|e| LoadError::Encode(format!("{e:#?}")))?;
            encoder
                .encode()
                .map_err(|e| LoadError::Encode(format!("{e:#?}")))?
        };

        if std::env::var_os("GOLDSRC_NO_AOT_CACHE").is_none() {
            let cache_dir = path
                .parent()
                .map(|p| p.join(".cache"))
                .unwrap_or_else(|| PathBuf::from(".cache"));
            let hash = compute_content_hash(&component_bytes);
            let cache_file = cache_dir.join(format!("{name}.{hash}.cwasm"));

            if cache_file.is_file() {
                // SAFETY: Loading previously cached component compiled by this engine.
                match unsafe { Component::deserialize_file(engine, &cache_file) } {
                    Ok(comp) => {
                        log::debug!(
                            target: log_targets::WASM,
                            "Loaded AOT precompiled component for '{name}' from {:?}",
                            cache_file
                        );
                        comp
                    }
                    Err(err) => {
                        log::warn!(
                            target: log_targets::WASM,
                            "Cached AOT component for '{name}' invalid ({err}); recompiling..."
                        );
                        let comp = Component::new(engine, &component_bytes)
                            .map_err(|e| LoadError::Compile(e.to_string()))?;
                        save_aot_cache(
                            engine,
                            &component_bytes,
                            &cache_dir,
                            &cache_file,
                            &name,
                            &hash,
                        );
                        comp
                    }
                }
            } else {
                let comp = Component::new(engine, &component_bytes)
                    .map_err(|e| LoadError::Compile(e.to_string()))?;
                save_aot_cache(
                    engine,
                    &component_bytes,
                    &cache_dir,
                    &cache_file,
                    &name,
                    &hash,
                );
                comp
            }
        } else {
            Component::new(engine, &component_bytes)
                .map_err(|e| LoadError::Compile(e.to_string()))?
        }
    };

    let mut linker = Linker::new(engine);
    api::add_to_linker::<HostState, wasmtime::component::HasSelf<HostState>>(
        &mut linker,
        |state: &mut HostState| state,
    )
    .map_err(|e| LoadError::Link(e.to_string()))?;

    let limits = wasmtime::StoreLimitsBuilder::new()
        .memory_size(64 * 1024 * 1024) // 64MB per memory
        .table_elements(10_000)
        .memories(4)
        .tables(16)
        .instances(16)
        .build();
    let state = HostState {
        engine: engine_ops.clone(),
        limits,
        plugin_name: String::new(),
        permissions: Vec::new(),
        shared_buckets: Vec::new(),
    };
    let mut store = wasmtime::Store::new(engine, state);
    store.limiter(|s| &mut s.limits);
    store.set_epoch_deadline(100);
    let bindings = GoldsrcPlugin::instantiate(&mut store, &component, &linker)
        .map_err(|e| LoadError::Instantiate(e.to_string()))?;

    let metadata = match bindings.call_get_metadata(&mut store) {
        Ok(meta_str) => {
            // First try parsing as full PluginManifest (has [plugin] table and [[commands]])
            let parsed_meta =
                if let Ok(manifest) = toml::from_str::<crate::plugin::PluginManifest>(&meta_str) {
                    let mut meta = manifest.plugin;
                    if !manifest.commands.is_empty() {
                        for cmd in &manifest.commands {
                            if !meta.commands.contains(&cmd.name) {
                                meta.commands.push(cmd.name.clone());
                            }
                        }
                        meta.command_defs = manifest.commands;
                    }
                    Some(meta)
                } else {
                    match toml::from_str::<PluginMetadata>(&meta_str) {
                        Ok(meta) => Some(meta),
                        Err(err) => {
                            crate::host_log(&format!(
                                "Warning: Failed to parse metadata for plugin at {:?}: {}",
                                path, err
                            ));
                            None
                        }
                    }
                };

            if let Some(mut meta) = parsed_meta {
                if let Some(ref b) = meta.bundle {
                    if b.is_empty()
                        || b.contains("..")
                        || b.starts_with('/')
                        || b.starts_with('\\')
                        || b.contains(':')
                        || !b
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/')
                    {
                        crate::host_log(&format!(
                            "Warning: Rejected invalid/unsafe bundle '{b}' for plugin at {:?}",
                            path
                        ));
                        meta.bundle = None;
                    } else if let Some(parent) = path.parent() {
                        // Check for bundle.toml in component directory or parent directory
                        let bundle_toml_path = parent.join("bundle.toml");
                        if bundle_toml_path.is_file()
                            && let Ok(manifest_content) = std::fs::read_to_string(&bundle_toml_path)
                        {
                            match toml::from_str::<goldsrc_api::bundle::BundleManifest>(
                                &manifest_content,
                            ) {
                                Ok(manifest) => {
                                    if let Err(err) = manifest.validate() {
                                        crate::host_log(&format!(
                                            "[Bundle Handshake] Warning: Invalid bundle manifest at {:?}: {}. Using binary defaults.",
                                            bundle_toml_path, err
                                        ));
                                    } else if let Some(spec) = manifest.components.get(&name) {
                                        if spec.role.is_coordinator()
                                            && let Some((coord_name, _)) = manifest.coordinator()
                                            && coord_name != name
                                        {
                                            crate::host_log(&format!(
                                                "[Bundle Handshake] Warning: Component '{}' in bundle '{}' cannot claim role 'coordinator'; bundle coordinator is '{}'. Retaining binary default '{}'.",
                                                name, b, coord_name, meta.role
                                            ));
                                        } else {
                                            meta.role = spec.role;
                                        }
                                    }
                                }
                                Err(err) => {
                                    crate::host_log(&format!(
                                        "[Bundle Handshake] Warning: Failed to parse {:?}: {}. Using binary defaults.",
                                        bundle_toml_path, err
                                    ));
                                }
                            }
                        }
                    }
                }
                Some(meta)
            } else {
                None
            }
        }
        Err(_) => None,
    };

    let shared_buckets = metadata
        .as_ref()
        .map(|m| m.shared_buckets.clone())
        .unwrap_or_default();

    let permissions = metadata
        .as_ref()
        .map(|m| m.permissions.clone())
        .unwrap_or_default();

    // Update HostState with validated plugin name, permissions, and shared buckets allowlist
    {
        let data = store.data_mut();
        data.plugin_name = name.clone();
        data.permissions = permissions;
        data.shared_buckets = shared_buckets;
    }

    Ok(LoadedPlugin {
        name,
        path: path.to_path_buf(),
        status: PluginStatus::Loaded,
        metadata,
        store,
        bindings,
        component,
    })
}
