//! Router and dispatcher for host server commands.

use crate::cli::handlers;
use crate::cli::response::CliResponse;
use crate::cli::specs::{find_command_spec, print_command_help, print_host_help};
use goldsrc_host_wasm::PluginManager;
use lexopt::Arg;

/// Calculates the Levenshtein edit distance between two ASCII strings using stack arrays without heap allocation.
pub fn levenshtein_distance(a: &str, b: &str) -> usize {
    const MAX_LEN: usize = 32;

    let mut a_bytes = a.as_bytes();
    let mut b_bytes = b.as_bytes();

    if a_bytes.len() >= MAX_LEN || b_bytes.len() >= MAX_LEN {
        return usize::MAX;
    }

    if a_bytes.len() < b_bytes.len() {
        std::mem::swap(&mut a_bytes, &mut b_bytes);
    }

    let b_len = b_bytes.len();
    if b_len == 0 {
        return a_bytes.len();
    }

    let mut dp = [0usize; MAX_LEN];
    for (j, item) in dp[..=b_len].iter_mut().enumerate() {
        *item = j;
    }

    for &ca in a_bytes {
        let mut prev_diag = dp[0];
        dp[0] += 1;

        for j in 0..b_len {
            let cb = b_bytes[j];
            let cost = usize::from(!ca.eq_ignore_ascii_case(&cb));

            let insertion = dp[j] + 1;
            let deletion = dp[j + 1] + 1;
            let substitution = prev_diag + cost;

            prev_diag = dp[j + 1];
            dp[j + 1] = insertion.min(deletion).min(substitution);
        }
    }

    dp[b_len]
}

/// Suggests the closest matching command name from registered specs, if any.
pub fn suggest_command(query: &str) -> Option<&'static str> {
    let mut best_match = None;
    let mut min_dist = usize::MAX;

    for spec in crate::cli::specs::BUILTIN_COMMANDS {
        let dist = levenshtein_distance(query, spec.name);
        if dist < min_dist {
            min_dist = dist;
            best_match = Some(spec.name);
        }
        for &alias in spec.aliases {
            let dist = levenshtein_distance(query, alias);
            if dist < min_dist {
                min_dist = dist;
                best_match = Some(spec.name);
            }
        }
    }

    if min_dist <= 2 { best_match } else { None }
}

/// Suggests the closest matching subcommand name from a list of candidates.
pub fn suggest_subcommand(query: &str, candidates: &[&'static str]) -> Option<&'static str> {
    let mut best_match = None;
    let mut min_dist = usize::MAX;

    for &cand in candidates {
        let dist = levenshtein_distance(query, cand);
        if dist < min_dist {
            min_dist = dist;
            best_match = Some(cand);
        }
    }

    if min_dist <= 2 { best_match } else { None }
}

fn with_manager_or_host<R>(
    manager: Option<&mut PluginManager>,
    f: impl FnOnce(Option<&mut PluginManager>) -> R,
) -> R {
    if let Some(m) = manager {
        f(Some(m))
    } else {
        crate::host::HostRuntime::with_manager(f)
    }
}

pub fn dispatch_host_command<F: FnMut(&str)>(
    raw_args: Vec<std::ffi::OsString>,
    manager: Option<&mut PluginManager>,
    version_info: (&str, &str, &str),
    mut out: F,
) {
    let (pkg_version, git_hash, build_target) = version_info;

    let mut parser = lexopt::Parser::from_args(raw_args);
    let _ = parser.next();

    let command_arg = match parser.next() {
        Ok(Some(Arg::Value(val))) => val.to_string_lossy().to_lowercase(),
        Ok(Some(Arg::Short('h') | Arg::Long("help"))) => {
            print_host_help(out);
            return;
        }
        _ => {
            print_host_help(out);
            return;
        }
    };

    if command_arg == "help" || command_arg == "?" {
        if let Ok(Some(Arg::Value(sub_name))) = parser.next() {
            let sub_query = sub_name.to_string_lossy();
            if let Some(spec) = find_command_spec(&sub_query) {
                print_command_help(spec, out);
            } else if crate::cli::specs::print_category_help(&sub_query, &mut out) {
                // Namespace/category help printed successfully.
            } else if let Some(suggestion) = suggest_command(&sub_query) {
                out(&format!(
                    "[GoldSrc.rs] Unknown command or namespace '{}'. Did you mean '{}'? Run 'grs help' for command list.\n",
                    sub_query, suggestion
                ));
            } else {
                out(&format!(
                    "[GoldSrc.rs] Unknown command or namespace '{}'. Run 'grs help' for command list.\n",
                    sub_query
                ));
            }
        } else {
            print_host_help(out);
        }
        return;
    }

    let Some(spec) = find_command_spec(&command_arg) else {
        if let Some(suggestion) = suggest_command(&command_arg) {
            out(&format!(
                "[GoldSrc.rs] Unknown command '{}'. Did you mean '{}'? Run 'grs help' for available commands.\n",
                command_arg, suggestion
            ));
        } else {
            out(&format!(
                "[GoldSrc.rs] Unknown command '{}'. Run 'grs help' for available commands.\n",
                command_arg
            ));
        }
        return;
    };

    match spec.name {
        "plugins" => {
            let sub_arg = match parser.next() {
                Ok(Some(Arg::Value(val))) => val.to_string_lossy().to_lowercase(),
                Ok(Some(Arg::Short('h') | Arg::Long("help"))) => {
                    print_command_help(spec, out);
                    return;
                }
                _ => {
                    print_command_help(spec, out);
                    return;
                }
            };

            let mut pause_overrides: Vec<(String, bool)> = Vec::new();

            with_manager_or_host(manager, |manager| match sub_arg.as_str() {
                "list" | "ls" | "ps" => handlers::handle_list(spec, parser, manager, &mut out),
                "info" => {
                    let mut targets = Vec::new();
                    let mut requested_field: Option<String> = None;
                    while let Ok(Some(arg)) = parser.next() {
                        match arg {
                            Arg::Short('h') | Arg::Long("help") => {
                                print_command_help(spec, out);
                                return;
                            }
                            Arg::Short('f') | Arg::Long("field") => {
                                if let Ok(val) = parser.value() {
                                    requested_field = Some(val.to_string_lossy().to_lowercase());
                                }
                            }
                            Arg::Value(val) => targets.push(val.to_string_lossy().into_owned()),
                            _ => {}
                        }
                    }
                    if targets.is_empty() {
                        out("[GoldSrc.rs] Usage: grs plugins info <name|index...> [-f <field>]\n");
                        return;
                    }
                    let Some(manager) = manager else {
                        out(&CliResponse::error("WASM Host not initialized.").format_console());
                        return;
                    };
                    for t in targets {
                        match manager.resolve_plugin_index(&t) {
                            Ok(idx) => {
                                let info = &manager.get_plugins_info()[idx];
                                let clean_path = crate::paths::PathResolver::normalize(&info.path);
                                let status_str = match &info.status {
                                    goldsrc_host_wasm::PluginStatus::Loaded => "Loaded".to_string(),
                                    goldsrc_host_wasm::PluginStatus::Running => {
                                        "Running".to_string()
                                    }
                                    goldsrc_host_wasm::PluginStatus::Paused { reason } => {
                                        if let Some(r) = reason {
                                            format!("Paused ({r})")
                                        } else {
                                            "Paused".to_string()
                                        }
                                    }
                                    goldsrc_host_wasm::PluginStatus::Blocked { reason } => {
                                        format!("Blocked ({reason})")
                                    }
                                    goldsrc_host_wasm::PluginStatus::Degraded { reason } => {
                                        format!("Degraded ({reason})")
                                    }
                                    goldsrc_host_wasm::PluginStatus::Poisoned { error } => {
                                        format!("Poisoned ({error})")
                                    }
                                    goldsrc_host_wasm::PluginStatus::Unloaded => {
                                        "Unloaded".to_string()
                                    }
                                };

                                let meta_name = info
                                    .metadata
                                    .as_ref()
                                    .map(|m| m.name.as_str())
                                    .unwrap_or(info.name.as_str());
                                let meta_version = info
                                    .metadata
                                    .as_ref()
                                    .map(|m| m.version.as_str())
                                    .unwrap_or(goldsrc_api::consts::DEFAULT_PLUGIN_VERSION);
                                let meta_author = info
                                    .metadata
                                    .as_ref()
                                    .map(|m| m.author.as_str())
                                    .unwrap_or(goldsrc_api::consts::DEFAULT_PLUGIN_AUTHOR);
                                let meta_desc = info
                                    .metadata
                                    .as_ref()
                                    .map(|m| m.description.as_str())
                                    .unwrap_or(goldsrc_api::consts::DEFAULT_PLUGIN_DESCRIPTION);
                                let meta_license = info
                                    .metadata
                                    .as_ref()
                                    .map(|m| m.license.as_str())
                                    .unwrap_or(goldsrc_api::consts::DEFAULT_PLUGIN_LICENSE);
                                let meta_url = info
                                    .metadata
                                    .as_ref()
                                    .map(|m| m.url.as_str())
                                    .unwrap_or(goldsrc_api::consts::DEFAULT_PLUGIN_URL);
                                let meta_systems = info
                                    .metadata
                                    .as_ref()
                                    .map(|m| {
                                        if m.systems.is_empty() {
                                            goldsrc_api::consts::DEFAULT_PLUGIN_SYSTEMS.to_string()
                                        } else {
                                            m.systems.join(", ")
                                        }
                                    })
                                    .unwrap_or_else(|| {
                                        goldsrc_api::consts::DEFAULT_PLUGIN_SYSTEMS.to_string()
                                    });
                                let meta_requires = info
                                    .metadata
                                    .as_ref()
                                    .map(|m| {
                                        if m.requires.is_empty() {
                                            goldsrc_api::consts::DEFAULT_PLUGIN_REQUIRES.to_string()
                                        } else {
                                            m.requires.join(", ")
                                        }
                                    })
                                    .unwrap_or_else(|| {
                                        goldsrc_api::consts::DEFAULT_PLUGIN_REQUIRES.to_string()
                                    });

                                if let Some(ref field) = requested_field {
                                    let value = match field.as_str() {
                                        "name" => meta_name,
                                        "version" => meta_version,
                                        "author" => meta_author,
                                        "description" => meta_desc,
                                        "license" => meta_license,
                                        "url" => meta_url,
                                        "path" => &clean_path,
                                        "status" => &status_str,
                                        "systems" => &meta_systems,
                                        "requires" | "require" => &meta_requires,
                                        "index" => {
                                            out(&format!("{}\n", info.index));
                                            continue;
                                        }
                                        other => {
                                            out(&format!(
                                                "[GoldSrc.rs] Unknown field '{}'. Supported: name, version, author, description, license, url, path, status, systems, requires, index.\n",
                                                other
                                            ));
                                            continue;
                                        }
                                    };
                                    out(&format!("{}\n", value));
                                    continue;
                                }

                                out(&format!("--- Plugin Info: {} ---\n", info.name));
                                out(&format!("  Index:        #{}\n", info.index));
                                out(&format!("  Path:         \"{}\"\n", clean_path));
                                out(&format!("  Status:       {}\n", status_str));
                                out(&format!("  Meta Name:    {}\n", meta_name));
                                out(&format!("  Version:      {}\n", meta_version));
                                out(&format!("  Author:       {}\n", meta_author));
                                out(&format!("  Description:  {}\n", meta_desc));
                                out(&format!("  License:      {}\n", meta_license));
                                out(&format!("  URL:          {}\n", meta_url));
                                out(&format!("  Systems:      {}\n", meta_systems));
                                out(&format!("  Requires:     {}\n", meta_requires));
                            }
                            Err(err) => {
                                out(&CliResponse::error(err.to_string()).format_console());
                            }
                        }
                    }
                }
                "load" => {
                    let mut targets = Vec::new();
                    while let Ok(Some(arg)) = parser.next() {
                        match arg {
                            Arg::Short('h') | Arg::Long("help") => {
                                print_command_help(spec, out);
                                return;
                            }
                            Arg::Value(val) => {
                                targets.push(val.to_string_lossy().into_owned());
                            }
                            _ => {}
                        }
                    }
                    if targets.is_empty() {
                        out("[GoldSrc.rs] Usage: grs plugins load <file1> [file2...]\n");
                        return;
                    }
                    let Some(manager) = manager else {
                        out(&CliResponse::error("WASM Host not initialized.").format_console());
                        return;
                    };
                    for t in targets {
                        match manager.load_plugin_by_name(&t) {
                            Ok(name) => out(&CliResponse::success(format!(
                                "Plugin '{name}' loaded successfully."
                            ))
                            .format_console()),
                            Err(err) => out(&CliResponse::error(err.to_string()).format_console()),
                        }
                    }
                }
                "unload" => {
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
                        out(&CliResponse::error("WASM Host not initialized.").format_console());
                        return;
                    };
                    if all {
                        let msg = manager.unload_all_plugins();
                        out(&CliResponse::success(msg).format_console());
                    } else if !targets.is_empty() {
                        for t in targets {
                            match manager.unload_plugin_by_query(&t) {
                                Ok(msg) => {
                                    out(&CliResponse::success(format!("{msg} successfully."))
                                        .format_console())
                                }
                                Err(err) => {
                                    out(&CliResponse::error(err.to_string()).format_console())
                                }
                            }
                        }
                    } else {
                        out("[GoldSrc.rs] Usage: grs plugins unload <name|index...> [-a|--all]\n");
                    }
                }
                "reload" | "rld" => handlers::handle_reload(spec, parser, manager, &mut out),
                "pause" => {
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
                        out(&CliResponse::error("WASM Host not initialized.").format_console());
                        return;
                    };
                    if all {
                        let outcome = manager.pause_all_plugins(true);
                        for info in manager.get_plugins_info() {
                            pause_overrides.push((info.name.clone(), true));
                        }
                        if outcome.changed > 0 {
                            out(&CliResponse::success(outcome.to_string()).format_console());
                        } else {
                            out(&CliResponse::notice(outcome.to_string()).format_console());
                        }
                    } else if !targets.is_empty() {
                        for t in targets {
                            match manager.pause_plugin(&t, true) {
                                Ok(outcome) => {
                                    pause_overrides.push((outcome.name().to_string(), true));
                                    if outcome.changed() {
                                        out(&CliResponse::success(outcome.to_string())
                                            .format_console());
                                    } else {
                                        out(&CliResponse::notice(outcome.to_string())
                                            .format_console());
                                    }
                                }
                                Err(err) => {
                                    out(&CliResponse::error(err.to_string()).format_console())
                                }
                            }
                        }
                    } else {
                        out("[GoldSrc.rs] Usage: grs plugins pause <name|index...> [-a|--all]\n");
                    }
                }
                "unpause" | "resume" | "unps" => {
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
                        out(&CliResponse::error("WASM Host not initialized.").format_console());
                        return;
                    };
                    if all {
                        let outcome = manager.pause_all_plugins(false);
                        for info in manager.get_plugins_info() {
                            pause_overrides.push((info.name.clone(), false));
                        }
                        if outcome.changed > 0 {
                            out(&CliResponse::success(outcome.to_string()).format_console());
                        } else {
                            out(&CliResponse::notice(outcome.to_string()).format_console());
                        }
                    } else if !targets.is_empty() {
                        for t in targets {
                            match manager.pause_plugin(&t, false) {
                                Ok(outcome) => {
                                    pause_overrides.push((outcome.name().to_string(), false));
                                    if outcome.changed() {
                                        out(&CliResponse::success(outcome.to_string())
                                            .format_console());
                                    } else {
                                        out(&CliResponse::notice(outcome.to_string())
                                            .format_console());
                                    }
                                }
                                Err(err) => {
                                    out(&CliResponse::error(err.to_string()).format_console())
                                }
                            }
                        }
                    } else {
                        out("[GoldSrc.rs] Usage: grs plugins unpause <name|index...> [-a|--all]\n");
                    }
                }
                "cmds" | "commands" => {
                    let mut positional = Vec::new();
                    while let Ok(Some(arg)) = parser.next() {
                        match arg {
                            Arg::Short('h') | Arg::Long("help") => {
                                print_command_help(spec, out);
                                return;
                            }
                            Arg::Value(v) => {
                                positional.push(v.to_string_lossy().to_string());
                            }
                            _ => {}
                        }
                    }
                    let Some(manager) = manager else {
                        out("[GoldSrc.rs] Error: WASM Host not initialized.\n");
                        return;
                    };
                    let plugins = manager.get_plugins_info();

                    if let Some(query) = positional.first() {
                        let query_clean = query.trim().to_ascii_lowercase();
                        let mut found_count = 0;

                        for p in &plugins {
                            if let Some(meta) = &p.metadata {
                                if !meta.command_defs.is_empty() {
                                    for cmd in &meta.command_defs {
                                        let name_matches =
                                            cmd.name.eq_ignore_ascii_case(&query_clean)
                                                || cmd
                                                    .name
                                                    .trim_start_matches(['/', '!'])
                                                    .eq_ignore_ascii_case(
                                                        query_clean.trim_start_matches(['/', '!']),
                                                    );
                                        let alias_matches = cmd.aliases.iter().any(|a| {
                                            a.eq_ignore_ascii_case(&query_clean)
                                                || a.trim_start_matches(['/', '!'])
                                                    .eq_ignore_ascii_case(
                                                        query_clean.trim_start_matches(['/', '!']),
                                                    )
                                        });

                                        if name_matches || alias_matches {
                                            found_count += 1;
                                            out(&format!("--- Command: {} ---\n", cmd.name));
                                            out(&format!(
                                                "  Plugin:      {} v{}\n",
                                                meta.name, meta.version
                                            ));
                                            if !cmd.description.is_empty() {
                                                out(&format!(
                                                    "  Description: {}\n",
                                                    cmd.description
                                                ));
                                            }
                                            let usage = if !cmd.usage.is_empty() {
                                                cmd.usage.as_str()
                                            } else {
                                                cmd.name.as_str()
                                            };
                                            out(&format!("  Usage:       {}\n", usage));
                                            if !cmd.aliases.is_empty() {
                                                out(&format!(
                                                    "  Aliases:     {}\n",
                                                    cmd.aliases.join(", ")
                                                ));
                                            }
                                            let access = cmd
                                                .capability
                                                .as_deref()
                                                .unwrap_or("Public (None)");
                                            out(&format!("  Access:      {}\n", access));
                                            out("\n");
                                        }
                                    }
                                } else if meta
                                    .commands
                                    .iter()
                                    .any(|c| c.eq_ignore_ascii_case(&query_clean))
                                {
                                    found_count += 1;
                                    out(&format!("--- Command: {} ---\n", query));
                                    out(&format!(
                                        "  Plugin:      {} v{}\n",
                                        meta.name, meta.version
                                    ));
                                    if !meta.description.is_empty() {
                                        out(&format!("  Description: {}\n", meta.description));
                                    }
                                    out(&format!("  Usage:       {}\n", query));
                                    out("  Access:      Public (None)\n\n");
                                }
                            }
                        }

                        if found_count == 0 {
                            out(&format!(
                                "[GoldSrc.rs] Command '{}' not found in any active plugin.\nType 'grs plugins cmds' to list all registered commands.\n",
                                query
                            ));
                        }
                        return;
                    }

                    let total_cmds: usize = plugins
                        .iter()
                        .filter_map(|p| {
                            p.metadata.as_ref().map(|m| {
                                if !m.command_defs.is_empty() {
                                    m.command_defs.len()
                                } else {
                                    m.commands.len()
                                }
                            })
                        })
                        .sum();

                    out(&format!(
                        "--- Registered Plugin Commands ({}) ---\n",
                        total_cmds
                    ));
                    if total_cmds == 0 {
                        out("  (No commands registered)\n");
                    } else {
                        for p in plugins {
                            if let Some(meta) = &p.metadata {
                                if !meta.command_defs.is_empty() {
                                    let desc = if !meta.description.is_empty() {
                                        format!(" - {}", meta.description)
                                    } else {
                                        String::new()
                                    };
                                    out(&format!(
                                        "\n[{name} v{ver}]{desc}\n",
                                        name = meta.name,
                                        ver = meta.version
                                    ));
                                    for cmd in &meta.command_defs {
                                        let usage_or_name = if !cmd.usage.is_empty() {
                                            &cmd.usage
                                        } else {
                                            &cmd.name
                                        };
                                        let aliases_str = if !cmd.aliases.is_empty() {
                                            format!(" (aliases: {})", cmd.aliases.join(", "))
                                        } else {
                                            String::new()
                                        };
                                        out(&format!("  * {}{}\n", usage_or_name, aliases_str));
                                        if !cmd.description.is_empty() || cmd.capability.is_some() {
                                            let cap_str = if let Some(ref cap) = cmd.capability {
                                                format!(" [requires: {}]", cap)
                                            } else {
                                                String::new()
                                            };
                                            out(&format!("    {}{}\n", cmd.description, cap_str));
                                        }
                                    }
                                } else if !meta.commands.is_empty() {
                                    let desc = if !meta.description.is_empty() {
                                        format!(" - {}", meta.description)
                                    } else {
                                        String::new()
                                    };
                                    out(&format!(
                                        "\n[{name} v{ver}]{desc}\n",
                                        name = meta.name,
                                        ver = meta.version
                                    ));
                                    for cmd in &meta.commands {
                                        out(&format!("  * {}\n", cmd));
                                    }
                                }
                            }
                        }
                    }
                }
                "-h" | "--help" | "help" => {
                    print_command_help(spec, out);
                }
                other => {
                    const PLUGIN_SUBCOMMANDS: &[&str] = &[
                        "list", "info", "load", "unload", "reload", "pause", "unpause", "cmds",
                    ];
                    if let Some(suggestion) = suggest_subcommand(other, PLUGIN_SUBCOMMANDS) {
                        out(&format!(
                            "[GoldSrc.rs] Unknown plugins subcommand '{}'. Did you mean '{}'? Run 'grs plugins --help' for usage.\n",
                            other, suggestion
                        ));
                    } else {
                        out(&format!(
                            "[GoldSrc.rs] Unknown plugins subcommand '{}'. Run 'grs plugins --help' for usage.\n",
                            other
                        ));
                    }
                }
            });

            for (plugin_name, is_paused) in pause_overrides {
                crate::host::HostRuntime::set_manual_pause_override(&plugin_name, is_paused);
            }
        }
        "watchers" => {
            let sub_arg = match parser.next() {
                Ok(Some(Arg::Value(val))) => val.to_string_lossy().to_lowercase(),
                Ok(Some(Arg::Short('h') | Arg::Long("help"))) => {
                    print_command_help(spec, out);
                    return;
                }
                _ => "list".to_string(),
            };

            match sub_arg.as_str() {
                "list" | "ls" => handlers::handle_watchers_list(spec, parser, out),
                "pause" => {
                    let mut target_id = None;
                    while let Ok(Some(arg)) = parser.next() {
                        match arg {
                            Arg::Short('h') | Arg::Long("help") => {
                                print_command_help(spec, out);
                                return;
                            }
                            Arg::Value(v) if target_id.is_none() => {
                                target_id = Some(v.to_string_lossy().into_owned());
                            }
                            _ => {}
                        }
                    }
                    if let Some(id) = target_id {
                        handlers::handle_watchers_pause(&id, out);
                    } else {
                        out(
                            "[GoldSrc.rs] Usage: grs watchers pause <id>\n  Example: grs watchers pause core:plugins\n",
                        );
                    }
                }
                "resume" | "unpause" => {
                    let mut target_id = None;
                    while let Ok(Some(arg)) = parser.next() {
                        match arg {
                            Arg::Short('h') | Arg::Long("help") => {
                                print_command_help(spec, out);
                                return;
                            }
                            Arg::Value(v) if target_id.is_none() => {
                                target_id = Some(v.to_string_lossy().into_owned());
                            }
                            _ => {}
                        }
                    }
                    if let Some(id) = target_id {
                        handlers::handle_watchers_resume(&id, out);
                    } else {
                        out(
                            "[GoldSrc.rs] Usage: grs watchers resume <id>\n  Example: grs watchers resume core:plugins\n",
                        );
                    }
                }
                "-h" | "--help" | "help" => {
                    print_command_help(spec, out);
                }
                other => {
                    const WATCHERS_SUBCOMMANDS: &[&str] = &["list", "pause", "resume"];
                    if let Some(suggestion) = suggest_subcommand(other, WATCHERS_SUBCOMMANDS) {
                        out(&format!(
                            "[GoldSrc.rs] Unknown watchers subcommand '{}'. Did you mean '{}'? Run 'grs watchers --help' for usage.\n",
                            other, suggestion
                        ));
                    } else {
                        out(&format!(
                            "[GoldSrc.rs] Unknown watchers subcommand '{}'. Run 'grs watchers --help' for usage.\n",
                            other
                        ));
                    }
                }
            }
        }
        "cmd" => {
            let mut cmd_name = None;
            let mut cmd_args = Vec::new();

            while let Ok(Some(arg)) = parser.next() {
                match arg {
                    Arg::Short('h') | Arg::Long("help") => {
                        print_command_help(spec, out);
                        return;
                    }
                    Arg::Value(val) => {
                        let s = val.to_string_lossy().into_owned();
                        if cmd_name.is_none() {
                            cmd_name = Some(s);
                        } else {
                            cmd_args.push(s);
                        }
                    }
                    _ => {}
                }
            }

            let Some(name) = cmd_name else {
                print_command_help(spec, out);
                return;
            };

            with_manager_or_host(manager, |manager| {
                let Some(manager) = manager else {
                    out("[GoldSrc.rs] Error: WASM Host not initialized.\n");
                    return;
                };

                let args_str = cmd_args.join(" ");
                let handled = manager.dispatch_command(&name, 0, &args_str);
                if !handled {
                    out(&CliResponse::warning(format!(
                        "Command '{name}' was not handled by any active plugin (is it paused or not registered?)."
                    )).format_console());
                }
            });
        }
        "exec" => {
            with_manager_or_host(manager, |manager| {
                handlers::handle_exec(spec, parser, manager, &mut out);
            });
        }
        "sessions" => {
            handlers::handle_sessions(spec, parser, out);
        }
        "cvars" => {
            handlers::handle_cvars(spec, parser, out);
        }
        "extensions" => {
            handlers::handle_extensions(spec, parser, out);
        }
        "hardware" => {
            handlers::handle_hardware(spec, parser, out);
        }
        "status" => {
            while let Ok(Some(arg)) = parser.next() {
                if let Arg::Short('h') | Arg::Long("help") = arg {
                    print_command_help(spec, out);
                    return;
                }
            }
            let plugins_count =
                with_manager_or_host(manager, |m| m.map(|mgr| mgr.loaded_count()).unwrap_or(0));
            let (watchers_total, watchers_active) = crate::HostRuntime::with_watcher_service(|w| {
                w.map(|s| {
                    let st = s.status();
                    (st.total, st.active)
                })
                .unwrap_or((0, 0))
            });
            let active_exts = crate::extension::extension_registry()
                .all()
                .into_iter()
                .filter(|e| e.is_available())
                .count();
            out("--- GoldSrc.rs Host Engine Status ---\n");
            out(&format!(
                "  Version:    v{} (git: {})\n",
                pkg_version, git_hash
            ));
            out(&format!("  Target:     {}\n", build_target));
            out("  WASM Engine: wasmtime (Component Model)\n");
            out(&format!("  Plugins:    {} loaded\n", plugins_count));
            out(&format!("  Extensions: {} active\n", active_exts));
            out(&format!(
                "  Watchers:   {} active ({} total registered)\n",
                watchers_active, watchers_total
            ));
        }
        "version" => {
            while let Ok(Some(arg)) = parser.next() {
                if let Arg::Short('h') | Arg::Long("help") = arg {
                    print_command_help(spec, out);
                    return;
                }
            }
            out(&format!(
                "GoldSrc.rs Host v{} (git: {})\nBuilt for target: {}\n",
                pkg_version, git_hash, build_target
            ));
        }
        _ => {
            print_host_help(out);
        }
    }
}
