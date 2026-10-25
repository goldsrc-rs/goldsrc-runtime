//! Handlers for specific CLI commands.

use crate::cli::specs::{CommandSpec, print_command_help};
use goldsrc_host_wasm::PluginManager;
use lexopt::Arg;

pub fn handle_list<F: FnMut(&str)>(
    spec: &CommandSpec,
    mut parser: lexopt::Parser,
    manager: Option<&mut PluginManager>,
    mut out: F,
) {
    let mut page: usize = 1;
    let mut size: Option<usize> = None;
    let mut only_paused = false;
    let mut all = false;
    let mut flat_view = false;
    while let Ok(Some(arg)) = parser.next() {
        match arg {
            Arg::Short('h') | Arg::Long("help") => {
                print_command_help(spec, out);
                return;
            }
            Arg::Short('p') | Arg::Long("page") => {
                if let Ok(val) = parser.value() {
                    page = val.to_string_lossy().parse().unwrap_or(1);
                }
            }
            Arg::Short('s') | Arg::Long("size") => {
                if let Ok(val) = parser.value() {
                    size = Some(val.to_string_lossy().parse().unwrap_or(5));
                }
            }
            Arg::Short('a') | Arg::Long("all") => all = true,
            Arg::Long("flat") => flat_view = true,
            Arg::Long("paused") => only_paused = true,
            _ => {}
        }
    }

    let Some(manager) = manager else {
        out("[GoldSrc.rs] Error: WASM Host not initialized.\n");
        return;
    };

    let mut plugins = manager.get_plugins_info();
    if only_paused {
        plugins.retain(|p| matches!(p.status, goldsrc_host_wasm::PluginStatus::Paused { .. }));
    }

    let total_plugins = plugins.len();
    if plugins.is_empty() {
        out("[GoldSrc.rs] WASM plugins (0):\n  (No plugins found)\n");
        return;
    }

    let has_bundles = plugins.iter().any(|p| {
        p.metadata
            .as_ref()
            .and_then(|m| m.bundle.as_ref())
            .is_some()
            || p.name.contains('/')
    });

    if !flat_view && has_bundles {
        out(&format!(
            "[GoldSrc.rs] WASM plugins ({} loaded):\n",
            total_plugins
        ));

        let mut root_plugins = Vec::new();
        let mut bundle_groups: std::collections::BTreeMap<
            String,
            Vec<&goldsrc_host_wasm::PluginInfo>,
        > = std::collections::BTreeMap::new();

        for p in &plugins {
            let bundle_name = p
                .metadata
                .as_ref()
                .and_then(|m| m.bundle.as_ref().cloned())
                .or_else(|| {
                    if let Some((b, _)) = p.name.split_once('/') {
                        Some(b.to_string())
                    } else {
                        None
                    }
                });

            if let Some(b) = bundle_name {
                bundle_groups.entry(b).or_default().push(p);
            } else {
                root_plugins.push(p);
            }
        }

        for p in root_plugins {
            let status = p.status.label();
            let version_str = p
                .metadata
                .as_ref()
                .map(|m| format!("v{}", m.version))
                .unwrap_or_else(|| "v1.0.0".to_string());
            let author_str = p
                .metadata
                .as_ref()
                .map(|m| {
                    if m.author.trim().is_empty() {
                        goldsrc_api::consts::DEFAULT_PLUGIN_AUTHOR
                    } else {
                        m.author.as_str()
                    }
                })
                .unwrap_or(goldsrc_api::consts::DEFAULT_PLUGIN_AUTHOR);

            out(&format!(
                "  [#{}] {:<16} {:<8} {:<18} | {:<7}\n",
                p.index, p.name, version_str, author_str, status
            ));
        }

        for (bundle_name, bundle_plugins) in bundle_groups {
            out(&format!(
                "  [{}] ({} plugins)\n",
                bundle_name,
                bundle_plugins.len()
            ));
            let count = bundle_plugins.len();
            for (i, p) in bundle_plugins.iter().enumerate() {
                let is_last = i + 1 == count;
                let branch = if is_last { "└── " } else { "├── " };
                let status = p.status.label();
                let version_str = p
                    .metadata
                    .as_ref()
                    .map(|m| format!("v{}", m.version))
                    .unwrap_or_else(|| "v1.0.0".to_string());
                let display_name = p
                    .name
                    .strip_prefix(&format!("{}/", bundle_name))
                    .unwrap_or(&p.name);
                let author_str = p
                    .metadata
                    .as_ref()
                    .map(|m| {
                        if m.author.trim().is_empty() {
                            goldsrc_api::consts::DEFAULT_PLUGIN_AUTHOR
                        } else {
                            m.author.as_str()
                        }
                    })
                    .unwrap_or(goldsrc_api::consts::DEFAULT_PLUGIN_AUTHOR);

                out(&format!(
                    "    {}[#{}] {:<14} {:<8} {:<18} | {:<7}\n",
                    branch, p.index, display_name, version_str, author_str, status
                ));
            }
        }
        return;
    }

    let page_size = if all {
        total_plugins.max(1)
    } else {
        size.unwrap_or(5)
    };
    let total_pages = (total_plugins + page_size - 1) / page_size.max(1);
    let page_idx = page.saturating_sub(1);

    out(&format!(
        "[GoldSrc.rs] WASM plugins ({}) [Page {}/{}]:\n",
        total_plugins,
        if total_pages == 0 { 1 } else { page },
        if total_pages == 0 { 1 } else { total_pages }
    ));

    let start = (page_idx * page_size).min(total_plugins);
    let end = (start + page_size).min(total_plugins);

    for p in &plugins[start..end] {
        let status = p.status.label();
        let version_str = p
            .metadata
            .as_ref()
            .map(|m| format!("v{}", m.version))
            .unwrap_or_else(|| "v1.0.0".to_string());
        let author_str = p
            .metadata
            .as_ref()
            .map(|m| {
                if m.author.trim().is_empty() {
                    goldsrc_api::consts::DEFAULT_PLUGIN_AUTHOR
                } else {
                    m.author.as_str()
                }
            })
            .unwrap_or(goldsrc_api::consts::DEFAULT_PLUGIN_AUTHOR);

        let desc_str = p
            .metadata
            .as_ref()
            .and_then(|m| {
                if m.description.is_empty() {
                    None
                } else {
                    Some(m.description.as_str())
                }
            })
            .unwrap_or("-");

        out(&format!(
            "  [#{}] {:<16} {:<8} {:<18} | {:<7} | {}\n",
            p.index, p.name, version_str, author_str, status, desc_str
        ));
    }
}

/// Handles `grs watchers list` inspection command.
pub fn handle_watchers_list<F: FnMut(&str)>(
    spec: &CommandSpec,
    mut parser: lexopt::Parser,
    mut out: F,
) {
    let mut as_json = false;
    while let Ok(Some(arg)) = parser.next() {
        match arg {
            Arg::Short('h') | Arg::Long("help") => {
                print_command_help(spec, out);
                return;
            }
            Arg::Long("json") => as_json = true,
            _ => {}
        }
    }

    let watchers = crate::HostRuntime::with_watcher_service(|w| {
        w.map(|service| service.list_watchers()).unwrap_or_default()
    });

    if as_json {
        let json = serde_json::to_string_pretty(&watchers).unwrap_or_else(|_| "[]".into());
        out(&json);
        out("\n");
        return;
    }

    if watchers.is_empty() {
        out("[GoldSrc.rs] Watchers (0):\n  (No registered filesystem watchers)\n");
        return;
    }

    out(&format!(
        "[GoldSrc.rs] Filesystem Watchers ({} registered):\n",
        watchers.len()
    ));
    for w in &watchers {
        let status_str = if w.is_paused { "[PAUSED]" } else { "[ACTIVE]" };
        let rec_str = if w.recursive { " (recursive)" } else { "" };
        out(&format!(
            "  - {:<16} {:<8} debounce: {}ms\n      target: {} \"{}\"{} (filter: {})\n",
            w.id,
            status_str,
            w.debounce_ms,
            w.target_type,
            crate::paths::PathResolver::normalize(&w.path),
            rec_str,
            w.filter_desc
        ));
    }
}

/// Handles `grs watchers pause <id>` command.
pub fn handle_watchers_pause<F: FnMut(&str)>(id: &str, mut out: F) {
    let res = crate::HostRuntime::with_watcher_service(|w| w.map(|s| s.pause(id)));
    match res {
        Some(true) => out(&format!("[GoldSrc.rs] Watcher '{id}' is now PAUSED.\n")),
        Some(false) => out(&format!("[GoldSrc.rs] Error: Watcher '{id}' not found.\n")),
        None => out("[GoldSrc.rs] Error: Watcher service not available.\n"),
    }
}

/// Handles `grs watchers resume <id>` command.
pub fn handle_watchers_resume<F: FnMut(&str)>(id: &str, mut out: F) {
    let res = crate::HostRuntime::with_watcher_service(|w| w.map(|s| s.resume(id)));
    match res {
        Some(true) => out(&format!("[GoldSrc.rs] Watcher '{id}' is now ACTIVE.\n")),
        Some(false) => out(&format!("[GoldSrc.rs] Error: Watcher '{id}' not found.\n")),
        None => out("[GoldSrc.rs] Error: Watcher service not available.\n"),
    }
}

/// Handles `grs plugins reload` and `grs rld` command.
pub fn handle_reload<F: FnMut(&str)>(
    spec: &CommandSpec,
    mut parser: lexopt::Parser,
    manager: Option<&mut PluginManager>,
    mut out: F,
) {
    let mut targets = Vec::new();
    let mut all = false;
    while let Ok(Some(arg)) = parser.next() {
        match arg {
            Arg::Short('h') | Arg::Long("help") => {
                print_command_help(spec, out);
                return;
            }
            Arg::Short('a') | Arg::Long("all") => all = true,
            Arg::Value(val) => targets.push(val.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    let Some(manager) = manager else {
        out(&crate::cli::CliResponse::error("WASM Host not initialized.").format_console());
        return;
    };
    if all {
        let msg = manager.reload_all_plugins();
        out(&crate::cli::CliResponse::success(msg).format_console());
    } else if !targets.is_empty() {
        for t in targets {
            match manager.reload_plugin_by_query(&t) {
                Ok(msg) => out(
                    &crate::cli::CliResponse::success(format!("{msg} successfully."))
                        .format_console(),
                ),
                Err(err) => out(&crate::cli::CliResponse::error(err.to_string()).format_console()),
            }
        }
    } else {
        out("[GoldSrc.rs] Usage: grs plugins reload <name|index...> [-a|--all]\n");
    }
}

pub fn handle_extensions<F: FnMut(&str)>(
    spec: &CommandSpec,
    mut parser: lexopt::Parser,
    mut out: F,
) {
    let mut subcommand = None;
    let mut target_name = None;

    while let Ok(Some(arg)) = parser.next() {
        match arg {
            Arg::Short('h') | Arg::Long("help") => {
                print_command_help(spec, out);
                return;
            }
            Arg::Value(val) => {
                let s = val.to_string_lossy().to_string();
                if subcommand.is_none() {
                    subcommand = Some(s);
                } else if target_name.is_none() {
                    target_name = Some(s);
                }
            }
            _ => {}
        }
    }

    let sub = subcommand.as_deref().unwrap_or("list");
    let registry = crate::extension::extension_registry();

    match sub {
        "list" | "ls" => {
            let mut extensions = registry.all();
            extensions.sort_by(|a, b| a.name().cmp(b.name()));
            if extensions.is_empty() {
                out("[GoldSrc.rs] Engine Extensions (0):\n  (No extensions registered)\n");
                return;
            }
            out(&format!(
                "[GoldSrc.rs] Engine Extensions ({}):\n",
                extensions.len()
            ));
            out(&format!(
                "  {:<12} {:<10} {:<10} {}\n",
                "NAME", "STATUS", "VERSION", "DESCRIPTION"
            ));
            out(&format!(
                "  {:-<12} {:-<10} {:-<10} {:-<30}\n",
                "", "", "", ""
            ));
            for ext in extensions {
                let status_str = if ext.is_available() {
                    "ACTIVE"
                } else {
                    "DISABLED"
                };
                out(&format!(
                    "  {:<12} {:<10} {:<10} {}\n",
                    ext.name(),
                    status_str,
                    ext.version(),
                    ext.description()
                ));
            }
        }
        "info" => {
            let Some(name) = target_name else {
                out("[GoldSrc.rs] Usage: grs extensions info <name>\n");
                return;
            };
            if let Some(ext) = registry.get(&name) {
                let status_str = if ext.is_available() {
                    "ACTIVE"
                } else {
                    "DISABLED"
                };
                out(&format!("[GoldSrc.rs] Extension: {}\n", ext.name()));
                out(&format!("  Name:        {}\n", ext.name()));
                out(&format!("  Status:      {}\n", status_str));
                out(&format!("  Version:     {}\n", ext.version()));
                out(&format!("  Description: {}\n", ext.description()));
            } else {
                out(&format!(
                    "[GoldSrc.rs] Error: Engine extension '{}' not found.\n",
                    name
                ));
            }
        }
        unknown => {
            out(&format!(
                "[GoldSrc.rs] Unknown extensions subcommand '{}'. Valid: list, info.\n",
                unknown
            ));
        }
    }
}

pub fn handle_hardware<F: FnMut(&str)>(spec: &CommandSpec, mut parser: lexopt::Parser, mut out: F) {
    let mut json_output = false;
    while let Ok(Some(arg)) = parser.next() {
        match arg {
            Arg::Short('h') | Arg::Long("help") => {
                print_command_help(spec, out);
                return;
            }
            Arg::Long("json") => {
                json_output = true;
            }
            _ => {}
        }
    }

    let snapshot = crate::hardware::system_info().sample_metrics();

    if json_output {
        let json = serde_json::json!({
            "cpu": {
                "brand": snapshot.cpu_brand,
                "vendor": snapshot.cpu_vendor,
                "physical_cores": snapshot.physical_cores,
                "logical_cores": snapshot.logical_cores,
                "global_usage_pct": snapshot.global_cpu_usage,
                "process_usage_pct": snapshot.process_cpu_usage
            },
            "memory": {
                "process_rss_mb": snapshot.process_memory_rss_bytes as f64 / (1024.0 * 1024.0),
                "process_virtual_mb": snapshot.process_memory_virtual_bytes as f64 / (1024.0 * 1024.0),
                "total_ram_mb": snapshot.total_memory_bytes as f64 / (1024.0 * 1024.0),
                "available_ram_mb": snapshot.available_memory_bytes as f64 / (1024.0 * 1024.0),
                "total_swap_mb": snapshot.total_swap_bytes as f64 / (1024.0 * 1024.0),
                "used_swap_mb": snapshot.used_swap_bytes as f64 / (1024.0 * 1024.0)
            },
            "performance": {
                "measured_fps": snapshot.measured_fps,
                "jitter_ms": snapshot.frame_jitter_ms,
                "uptime_secs": snapshot.uptime_secs
            }
        });
        out(&format!("{}\n", json));
        return;
    }

    let rss_mb = snapshot.process_memory_rss_bytes as f64 / (1024.0 * 1024.0);
    let total_ram_mb = snapshot.total_memory_bytes as f64 / (1024.0 * 1024.0);
    let avail_ram_mb = snapshot.available_memory_bytes as f64 / (1024.0 * 1024.0);
    let phys_str = snapshot
        .physical_cores
        .map(|c| c.to_string())
        .unwrap_or_else(|| "N/A".to_string());

    out("====================================================\n");
    out("       GoldSrc.rs Host Hardware & Diagnostics       \n");
    out("====================================================\n");
    out(&format!(
        "  CPU Model     : {}\n",
        snapshot.cpu_brand.trim()
    ));
    out(&format!("  Vendor / Arch : {}\n", snapshot.cpu_vendor));
    out(&format!(
        "  Cores / Threads: {} physical / {} logical\n",
        phys_str, snapshot.logical_cores
    ));
    out(&format!(
        "  Host CPU Load : {:.1}%\n",
        snapshot.global_cpu_usage
    ));
    out(&format!(
        "  HLDS CPU Load : {:.1}%\n",
        snapshot.process_cpu_usage
    ));
    out("----------------------------------------------------\n");
    out(&format!("  HLDS Process  : {:.1} MB RSS\n", rss_mb));
    out(&format!(
        "  Host RAM Free : {:.0} MB / {:.0} MB total ({:.1}% free)\n",
        avail_ram_mb,
        total_ram_mb,
        (avail_ram_mb / total_ram_mb.max(1.0)) * 100.0
    ));
    out("----------------------------------------------------\n");
    let fps_display = if snapshot.measured_fps > 0.0 {
        format!("{:.1} FPS", snapshot.measured_fps)
    } else {
        "Calibrating...".to_string()
    };
    out(&format!("  Engine Rate   : {}\n", fps_display));
    out(&format!(
        "  Frame Jitter  : {:.2} ms\n",
        snapshot.frame_jitter_ms
    ));
    out(&format!("  Host Uptime   : {}s\n", snapshot.uptime_secs));
    out("====================================================\n");
}

pub fn handle_sessions<F: FnMut(&str)>(spec: &CommandSpec, mut parser: lexopt::Parser, mut out: F) {
    let sub_arg = match parser.next() {
        Ok(Some(Arg::Value(val))) => val.to_string_lossy().to_lowercase(),
        Ok(Some(Arg::Short('h') | Arg::Long("help"))) => {
            print_command_help(spec, out);
            return;
        }
        _ => "list".to_string(),
    };

    match sub_arg.as_str() {
        "list" | "ls" => {
            let mut format = "plain".to_string();
            let mut verbose = false;

            while let Ok(Some(arg)) = parser.next() {
                match arg {
                    Arg::Short('h') | Arg::Long("help") => {
                        print_command_help(spec, out);
                        return;
                    }
                    Arg::Short('v') | Arg::Long("verbose") => verbose = true,
                    Arg::Short('o') | Arg::Long("format") => {
                        if let Ok(val) = parser.value() {
                            format = val.to_string_lossy().to_lowercase();
                        }
                    }
                    _ => {}
                }
            }

            let sessions = crate::host::HostRuntime::with_sessions(|mgr| {
                mgr.active_sessions()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

            if format == "json" {
                let json_list: Vec<serde_json::Value> = sessions
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "slot": s.slot,
                            "user_id": s.user_id,
                            "generation": s.generation,
                            "lang": s.lang(),
                            "overrides_count": s.userinfo_overrides.len(),
                            "metadata_count": s.metadata.len(),
                        })
                    })
                    .collect();
                let json = serde_json::json!({
                    "total": json_list.len(),
                    "sessions": json_list
                });
                out(&format!("{}\n", json));
                return;
            }

            out(&format!(
                "--- Active Client Sessions ({}) ---\n",
                sessions.len()
            ));
            if sessions.is_empty() {
                out("  (No active client sessions)\n");
            } else {
                out("  Slot  UserID  Gen  Lang     Userinfo Overrides\n");
                out("  ------------------------------------------------------------\n");
                for s in sessions {
                    let lang_str = s.lang().unwrap_or("-");
                    let overrides_summary = if verbose {
                        s.userinfo_overrides
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else if s.userinfo_overrides.is_empty() {
                        "-".to_string()
                    } else {
                        format!("{} keys", s.userinfo_overrides.len())
                    };

                    out(&format!(
                        "  #{:<4} #{:<6} {:<4} {:<8} {}\n",
                        s.slot, s.user_id, s.generation, lang_str, overrides_summary
                    ));
                }
            }
        }
        "info" => {
            let mut target_slot: Option<i32> = None;
            let mut field: Option<String> = None;
            let mut format = "plain".to_string();
            let mut verbose = false;

            while let Ok(Some(arg)) = parser.next() {
                match arg {
                    Arg::Short('h') | Arg::Long("help") => {
                        print_command_help(spec, out);
                        return;
                    }
                    Arg::Short('f') | Arg::Long("field") => {
                        if let Ok(val) = parser.value() {
                            field = Some(val.to_string_lossy().to_lowercase());
                        }
                    }
                    Arg::Short('o') | Arg::Long("format") => {
                        if let Ok(val) = parser.value() {
                            format = val.to_string_lossy().to_lowercase();
                        }
                    }
                    Arg::Short('v') | Arg::Long("verbose") => verbose = true,
                    Arg::Value(val) if target_slot.is_none() => {
                        target_slot = val.to_string_lossy().parse().ok();
                    }
                    _ => {}
                }
            }

            let Some(slot) = target_slot else {
                out("[GoldSrc.rs] Usage: grs sessions info <slot> [-f field] [-v] [-o format]\n");
                return;
            };

            let session =
                crate::host::HostRuntime::with_sessions(|mgr| mgr.get(slot).cloned()).flatten();

            let Some(sess) = session else {
                out(&format!(
                    "[GoldSrc.rs] Error: No active session found for slot #{slot}.\n"
                ));
                return;
            };

            if let Some(ref f) = field {
                match f.as_str() {
                    "slot" => out(&format!("{}\n", sess.slot)),
                    "userid" | "user_id" => out(&format!("{}\n", sess.user_id)),
                    "gen" | "generation" => out(&format!("{}\n", sess.generation)),
                    "lang" => out(&format!("{}\n", sess.lang().unwrap_or(""))),
                    "token" => {
                        let t = sess.token();
                        out(&format!(
                            "{}:{}:{}\n",
                            t.slot(),
                            t.generation(),
                            t.user_id()
                        ));
                    }
                    custom_key => {
                        if let Some(val) = sess.get_userinfo(custom_key) {
                            out(&format!("{}\n", val));
                        } else if let Some(val) = sess.metadata.get(custom_key) {
                            out(&format!("{}\n", val));
                        } else {
                            out(&format!("[Field '{}' not set]\n", custom_key));
                        }
                    }
                }
                return;
            }

            if format == "json" {
                let json = serde_json::json!({
                    "slot": sess.slot,
                    "user_id": sess.user_id,
                    "generation": sess.generation,
                    "token": {
                        "slot": sess.token().slot(),
                        "generation": sess.token().generation(),
                        "user_id": sess.token().user_id(),
                    },
                    "lang": sess.lang(),
                    "userinfo_overrides": sess.userinfo_overrides,
                    "metadata": sess.metadata,
                });
                out(&format!("{}\n", json));
                return;
            }

            out(&format!("--- Client Session [Slot #{}] ---\n", sess.slot));
            out(&format!("  User ID:       #{}\n", sess.user_id));
            out(&format!("  Generation:    {}\n", sess.generation));
            let tok = sess.token();
            out(&format!(
                "  Session Token: slot={}, gen={}, uid={}\n",
                tok.slot(),
                tok.generation(),
                tok.user_id()
            ));
            out(&format!(
                "  Language:      {}\n",
                sess.lang().unwrap_or("default")
            ));
            out(&format!(
                "  Overrides ({}):\n",
                sess.userinfo_overrides.len()
            ));
            if sess.userinfo_overrides.is_empty() {
                out("    (None)\n");
            } else {
                for (k, v) in &sess.userinfo_overrides {
                    out(&format!("    * {:<16} = {}\n", k, v));
                }
            }

            if verbose || !sess.metadata.is_empty() {
                out(&format!("  Metadata ({}):\n", sess.metadata.len()));
                if sess.metadata.is_empty() {
                    out("    (None)\n");
                } else {
                    for (k, v) in &sess.metadata {
                        out(&format!("    * {:<16} = {}\n", k, v));
                    }
                }
            }
        }
        _ => {
            print_command_help(spec, out);
        }
    }
}

pub fn handle_cvars<F: FnMut(&str)>(spec: &CommandSpec, mut parser: lexopt::Parser, mut out: F) {
    let sub_arg = match parser.next() {
        Ok(Some(Arg::Value(val))) => val.to_string_lossy().to_lowercase(),
        Ok(Some(Arg::Short('h') | Arg::Long("help"))) => {
            print_command_help(spec, out);
            return;
        }
        _ => "list".to_string(),
    };

    match sub_arg.as_str() {
        "list" | "ls" => {
            let mut pattern: Option<String> = None;
            let mut only_diff = false;
            let mut format = "plain".to_string();

            while let Ok(Some(arg)) = parser.next() {
                match arg {
                    Arg::Short('h') | Arg::Long("help") => {
                        print_command_help(spec, out);
                        return;
                    }
                    Arg::Short('d') | Arg::Long("diff") => only_diff = true,
                    Arg::Short('o') | Arg::Long("format") => {
                        if let Ok(val) = parser.value() {
                            format = val.to_string_lossy().to_lowercase();
                        }
                    }
                    Arg::Value(val) if pattern.is_none() => {
                        pattern = Some(val.to_string_lossy().to_lowercase());
                    }
                    _ => {}
                }
            }

            let mut cvars = crate::backend::engine_bridge::cvars::get_registered_cvars();
            if only_diff {
                cvars.retain(|c| c.is_modified());
            }
            if let Some(ref pat) = pattern {
                cvars.retain(|c| c.name.to_ascii_lowercase().contains(pat));
            }

            if format == "json" {
                let json_list: Vec<serde_json::Value> = cvars
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "name": c.name,
                            "current_value": c.current_value,
                            "default_value": c.default_value,
                            "current_float": c.current_float,
                            "flags": format!("0x{:04x}", c.flags),
                            "is_modified": c.is_modified(),
                        })
                    })
                    .collect();
                let json = serde_json::json!({
                    "total": json_list.len(),
                    "diff_only": only_diff,
                    "cvars": json_list
                });
                out(&format!("{}\n", json));
                return;
            }

            let header = if only_diff {
                format!("--- Modified Console Variables ({}) ---", cvars.len())
            } else {
                format!("--- Host Console Variables ({}) ---", cvars.len())
            };
            out(&format!("{}\n", header));

            if cvars.is_empty() {
                out("  (No cvars matched query)\n");
            } else {
                out("  Name                      Value            Default          Flags\n");
                out("  ----------------------------------------------------------------------\n");
                for c in cvars {
                    out(&format!(
                        "  {:<25} {:<16} {:<16} 0x{:04x}\n",
                        c.name, c.current_value, c.default_value, c.flags
                    ));
                }
            }
        }
        "info" => {
            let mut target_name: Option<String> = None;
            let mut field: Option<String> = None;
            let mut format = "plain".to_string();

            while let Ok(Some(arg)) = parser.next() {
                match arg {
                    Arg::Short('h') | Arg::Long("help") => {
                        print_command_help(spec, out);
                        return;
                    }
                    Arg::Short('f') | Arg::Long("field") => {
                        if let Ok(val) = parser.value() {
                            field = Some(val.to_string_lossy().to_lowercase());
                        }
                    }
                    Arg::Short('o') | Arg::Long("format") => {
                        if let Ok(val) = parser.value() {
                            format = val.to_string_lossy().to_lowercase();
                        }
                    }
                    Arg::Value(val) if target_name.is_none() => {
                        target_name = Some(val.to_string_lossy().into_owned());
                    }
                    _ => {}
                }
            }

            let Some(name) = target_name else {
                out("[GoldSrc.rs] Usage: grs cvars info <name> [-f field] [-o format]\n");
                return;
            };

            let Some(info) = crate::backend::engine_bridge::cvars::query_cvar_info(&name) else {
                out(&format!(
                    "[GoldSrc.rs] Error: Console variable '{name}' not found.\n"
                ));
                return;
            };

            if let Some(ref f) = field {
                match f.as_str() {
                    "name" => out(&format!("{}\n", info.name)),
                    "value" | "val" | "current" => out(&format!("{}\n", info.current_value)),
                    "default" | "def" => out(&format!("{}\n", info.default_value)),
                    "float" => out(&format!("{}\n", info.current_float)),
                    "flags" => out(&format!("0x{:04x}\n", info.flags)),
                    "modified" => out(&format!("{}\n", info.is_modified())),
                    other => out(&format!("[Unknown cvar field '{other}']\n")),
                }
                return;
            }

            if format == "json" {
                let json = serde_json::json!({
                    "name": info.name,
                    "current_value": info.current_value,
                    "default_value": info.default_value,
                    "current_float": info.current_float,
                    "flags": format!("0x{:04x}", info.flags),
                    "is_modified": info.is_modified(),
                });
                out(&format!("{}\n", json));
                return;
            }

            out(&format!("--- Cvar Info: {} ---\n", info.name));
            out(&format!("  Current Value: {}\n", info.current_value));
            out(&format!("  Default Value: {}\n", info.default_value));
            out(&format!("  Numeric Float: {}\n", info.current_float));
            out(&format!("  Flags:         0x{:04x}\n", info.flags));
            out(&format!(
                "  Modified:      {}\n",
                if info.is_modified() { "yes" } else { "no" }
            ));
        }
        _ => {
            print_command_help(spec, out);
        }
    }
}

/// Handler for `grs exec <preset>` and `grs exec --restore`.
pub fn handle_exec<F: FnMut(&str)>(
    spec: &CommandSpec,
    mut parser: lexopt::Parser,
    manager: Option<&mut PluginManager>,
    mut out: F,
) {
    let mut restore = false;
    let mut preset_target: Option<String> = None;

    while let Ok(Some(arg)) = parser.next() {
        match arg {
            Arg::Short('h') | Arg::Long("help") => {
                print_command_help(spec, out);
                return;
            }
            Arg::Long("restore") | Arg::Short('r') => {
                restore = true;
            }
            Arg::Value(val) if preset_target.is_none() => {
                preset_target = Some(val.to_string_lossy().into_owned());
            }
            _ => {}
        }
    }

    if restore {
        let Some(snapshot) = crate::preset::PresetEngine::take_snapshot() else {
            out("[GoldSrc.rs] Error: No active state snapshot found to restore.\n");
            return;
        };

        let mut cvars_reverted = 0;
        let mut plugins_reverted = 0;

        // Revert CVARs
        if let Some(engine) = crate::host::HostRuntime::engine() {
            for (cvar_name, original_val) in &snapshot.original_cvars {
                engine.cvar_set_string(cvar_name, original_val);
                cvars_reverted += 1;
            }
        }

        // Revert plugin states
        if let Some(mgr) = manager {
            for (pl_name, was_paused) in &snapshot.original_plugin_states {
                if *was_paused {
                    let _ = mgr.pause_plugin_with_reason(
                        pl_name,
                        true,
                        Some("Preset rollback".to_string()),
                    );
                } else {
                    let _ = mgr.pause_plugin(pl_name, false);
                }
                plugins_reverted += 1;
            }
        }

        out(&format!(
            "[GoldSrc.rs] Successfully restored state from preset '{}' ({} cvars reverted, {} plugins reverted).\n",
            snapshot.preset_name, cvars_reverted, plugins_reverted
        ));
        return;
    }

    let Some(target) = preset_target else {
        out("[GoldSrc.rs] Usage: grs exec <preset_name> [--restore]\n");
        return;
    };

    let backend = crate::host::HostRuntime::backend_type();
    let presets_dir = crate::paths::PathResolver::framework_dir(backend).join("presets");

    let resolved_path = crate::preset::PresetEngine::resolve_preset_path(&target, &presets_dir);
    let manifest = match crate::preset::PresetEngine::load_manifest(&resolved_path) {
        Ok(m) => m,
        Err(e) => {
            out(&format!("[GoldSrc.rs] Error loading preset: {e}\n"));
            return;
        }
    };

    // Capture pre-flight snapshot before mutations
    let snapshot = crate::preset::PresetEngine::capture_snapshot(
        &manifest,
        |cvar| crate::host::HostRuntime::engine().and_then(|eng| eng.cvar_get_string(cvar)),
        |pl| {
            if let Some(ref mgr) = manager {
                mgr.resolve_plugin_index(pl).ok().map(|idx| {
                    matches!(
                        mgr.get_plugins_info()[idx].status,
                        goldsrc_host_wasm::PluginStatus::Paused { .. }
                    )
                })
            } else {
                None
            }
        },
    );

    crate::preset::PresetEngine::store_snapshot(snapshot);

    let mut cvars_applied = 0;
    if let Some(engine) = crate::host::HostRuntime::engine() {
        for (cvar, val) in &manifest.cvars {
            engine.cvar_set_string(cvar, val);
            cvars_applied += 1;
        }
    }

    let mut plugins_paused = 0;
    let mut plugins_resumed = 0;

    if let Some(mgr) = manager {
        for pl in &manifest.plugins.pause {
            if mgr
                .pause_plugin_with_reason(pl, true, Some("Preset execution".to_string()))
                .is_ok()
            {
                plugins_paused += 1;
            }
        }
        for pl in &manifest.plugins.resume {
            if mgr.pause_plugin(pl, false).is_ok() {
                plugins_resumed += 1;
            }
        }
    }

    out(&format!(
        "[GoldSrc.rs] Executed preset '{}' ({} cvars applied, {} plugins paused, {} plugins resumed).\n",
        manifest.metadata.name, cvars_applied, plugins_paused, plugins_resumed
    ));
    out("  Run 'grs exec --restore' to return to previous state.\n");
}
