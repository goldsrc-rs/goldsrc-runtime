//! Dependency resolution and lifecycle state transitions.

use crate::plugin::{LoadedPlugin, PluginStatus};
use goldsrc_spi::engine::EnginePort as GoldsrcEngine;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

/// Recalculates `status` across all loaded plugins according to dependency states.
pub fn recalculate_dependency_states(
    plugins: &mut [LoadedPlugin],
    engine_ops: &Arc<dyn GoldsrcEngine>,
) {
    let mut loaded_plugins = HashMap::new();
    let mut running_plugins = HashMap::new();

    for p in plugins.iter() {
        if matches!(
            p.status,
            PluginStatus::Running | PluginStatus::Paused { .. } | PluginStatus::Loaded
        ) {
            let ver = p
                .metadata
                .as_ref()
                .map(|m| m.version.clone())
                .unwrap_or_else(|| "1.0.0".to_string());
            loaded_plugins.insert(p.name.clone(), ver.clone());
            if !matches!(p.status, PluginStatus::Paused { .. }) {
                running_plugins.insert(p.name.clone(), ver);
            }
        }
    }

    for plugin in plugins.iter_mut() {
        if matches!(plugin.status, PluginStatus::Poisoned { .. }) {
            continue;
        }

        let mut missing_dep = None;
        let mut paused_dep = None;

        if let Some(meta) = &plugin.metadata {
            for req_str in &meta.requires {
                if let Ok(req) = goldsrc_api::Requirement::from_str(req_str) {
                    match req {
                        goldsrc_api::Requirement::Plugin {
                            name,
                            version_req,
                            optional,
                        } => {
                            if let Some(loaded_ver) = loaded_plugins.get(&name) {
                                if let Some(ref req_str) = version_req {
                                    let satisfies = semver::VersionReq::parse(req_str)
                                        .and_then(|r| {
                                            semver::Version::parse(loaded_ver)
                                                .map(|v| r.matches(&v))
                                        })
                                        .unwrap_or(loaded_ver == req_str);
                                    if !satisfies && !optional {
                                        missing_dep = Some(format!(
                                            "plugin dependency '{name}' version mismatch (got {loaded_ver}, requires {req_str})"
                                        ));
                                        break;
                                    }
                                }
                                if !running_plugins.contains_key(&name) && !optional {
                                    paused_dep =
                                        Some(format!("waiting for paused plugin '{name}'"));
                                }
                            } else if !optional {
                                missing_dep = Some(format!("missing plugin dependency '{name}'"));
                                break;
                            }
                        }
                        goldsrc_api::Requirement::Extension {
                            name,
                            version_req,
                            optional,
                        } => {
                            let available =
                                engine_ops.is_extension_available(&name, version_req.as_deref());
                            if !available && !optional {
                                let req_desc = if let Some(ref ver) = version_req {
                                    format!("'ext:{name}@{ver}'")
                                } else {
                                    format!("'ext:{name}'")
                                };
                                missing_dep = Some(format!("missing engine extension {req_desc}"));
                                break;
                            }
                        }
                        goldsrc_api::Requirement::Cvar { name, op } => {
                            let cvar_val = engine_ops.cvar_get_string(&name).unwrap_or_default();
                            let satisfied = match op {
                                goldsrc_api::CvarOp::Equal(expected) => cvar_val == expected,
                                goldsrc_api::CvarOp::NotEqual(forbidden) => cvar_val != forbidden,
                                goldsrc_api::CvarOp::GreaterThanZero => {
                                    cvar_val.parse::<f32>().map(|v| v > 0.0).unwrap_or(false)
                                }
                            };
                            if !satisfied {
                                missing_dep = Some(format!(
                                    "cvar requirement '{name}' not satisfied (current: '{cvar_val}')"
                                ));
                                break;
                            }
                        }
                        goldsrc_api::Requirement::Feature { name } => {
                            let token = goldsrc_api::FeatureToken::from_name(&name);
                            if !engine_ops.has_feature(token.raw()) {
                                missing_dep =
                                    Some(format!("missing required game feature '{name}'"));
                                break;
                            }
                        }
                        goldsrc_api::Requirement::Backend { id: _ } => {}
                        _ => {}
                    }
                }
            }
        }

        if let Some(reason) = missing_dep {
            plugin.status = PluginStatus::Blocked { reason };
        } else if let Some(reason) = paused_dep {
            plugin.status = PluginStatus::Degraded { reason };
        } else if matches!(
            plugin.status,
            PluginStatus::Blocked { .. } | PluginStatus::Degraded { .. }
        ) {
            plugin.status = PluginStatus::Running;
        }
    }
}
