//! Host CLI dispatch, C-ABI bindings, and declarative commands for GoldSrc.rs.

pub mod handlers;
pub mod pipeline;
pub mod response;
pub mod router;
pub mod specs;

pub use pipeline::{
    CommandDomain, ConsolePipelinePreprocessor, ExecutionPlan, PipeChain, PipelinedCommand,
    Redirection,
};
pub use response::{CliResponse, CommandStatus};
pub use router::dispatch_host_command;
pub use specs::{
    BUILTIN_CATEGORIES, BUILTIN_COMMANDS, CommandSpec, find_command_spec, print_category_help,
    print_command_help, print_host_help,
};

use std::ffi::{OsString, c_char};
use std::sync::OnceLock;

/// Backend accessors needed to run the host CLI as a server command.
pub struct HostCliBackend {
    /// Returns the current engine-provided argc.
    pub argc: fn() -> i32,
    /// Returns the engine-provided argv entry at `i`.
    pub argv: fn(i32) -> *const c_char,
    /// Prints a line to the server console.
    pub print: fn(&str),
    /// `(package_version, git_hash, build_target)`.
    pub version: (&'static str, &'static str, &'static str),
}

static HOST_CLI: OnceLock<HostCliBackend> = OnceLock::new();

/// Initialize the shared host CLI backend accessors. Call once at backend init.
pub fn init_host_cli(backend: HostCliBackend) {
    let _ = HOST_CLI.set(backend);
}

/// Safely decodes a raw C-string pointer into a UTF-8 String.
/// Attempts standard UTF-8 parsing first; falls back to CP1251 (Cyrillic) decoding
/// if non-UTF-8 bytes (like Cyrillic console input) are present.
///
/// # Safety
/// If `ptr` is non-null, it must point to valid, readable memory containing a NUL terminator.
pub unsafe fn decode_c_string_lossy(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // Bounded scan to prevent buffer over-reads if NUL terminator is missing
    let len = unsafe { goldsrc_sys::ffi::libc_strnlen(ptr, 4096) };
    if len == 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    if let Ok(s) = std::str::from_utf8(bytes) {
        s.to_string()
    } else {
        goldsrc_api::cp1251_to_utf8(bytes)
    }
}

/// Shared server-command handler for `meta-rs` / `mrs` / `grs`.
///
/// # Safety
/// Registered as a C server command; the engine provides the argv accessors.
pub unsafe extern "C" fn handle_host_command() {
    goldsrc_sys::ffi::catch_ffi_panic("handle_host_command", (), || {
        let Some(backend) = HOST_CLI.get() else {
            return;
        };
        let argc = (backend.argc)();
        if argc == 0 {
            return;
        }
        let mut raw_args = Vec::new();
        for i in 0..argc {
            let arg_ptr = (backend.argv)(i);
            if !arg_ptr.is_null() {
                let decoded = unsafe { decode_c_string_lossy(arg_ptr) };
                raw_args.push(OsString::from(decoded));
            }
        }
        // Check if raw arguments contain pipeline operators (`|`, `&&`, `>`, `<`).
        let raw_line = raw_args
            .iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");

        if ConsolePipelinePreprocessor::has_pipeline_operators(&raw_line) {
            let backend_type = crate::host::HostRuntime::backend_type();
            let config = crate::config::HostConfig::load_or_create(backend_type);
            match ConsolePipelinePreprocessor::parse(
                &raw_line,
                CommandDomain::Server,
                &config.pipeline,
            ) {
                Ok(plan) => {
                    let _ = ConsolePipelinePreprocessor::execute_plan(
                        &plan,
                        CommandDomain::Server,
                        |cmd_args, _piped_input| {
                            let mut buf = String::new();
                            let os_args: Vec<OsString> =
                                cmd_args.iter().map(OsString::from).collect();
                            dispatch_host_command(os_args, None, backend.version, |chunk| {
                                buf.push_str(chunk);
                            });
                            let status = if buf.contains("[GoldSrc.rs] Error")
                                || buf.contains("Unknown command")
                            {
                                CommandStatus::Error
                            } else {
                                CommandStatus::Success
                            };
                            (status, buf)
                        },
                        backend.print,
                    );
                    return;
                }
                Err(err) => {
                    (backend.print)(&format!("[GoldSrc.rs] Pipeline syntax error: {err}\n"));
                    return;
                }
            }
        }

        // Dispatch directly; commands requiring PluginManager will acquire it with
        // narrow scope, preventing re-entrant deadlocks with WatcherService or HostRuntime.
        dispatch_host_command(raw_args, None, backend.version, backend.print);
    });
}

/// Shared server-command handler for WASM plugin commands.
///
/// # Safety
/// Registered as a C server command via `pfnAddServerCommand`.
pub unsafe extern "C" fn handle_plugin_server_command() {
    goldsrc_sys::ffi::catch_ffi_panic("handle_plugin_server_command", (), || {
        let Some(backend) = HOST_CLI.get() else {
            return;
        };
        let argc = (backend.argc)();
        if argc == 0 {
            return;
        }
        let name_ptr = (backend.argv)(0);
        if name_ptr.is_null() {
            return;
        }
        let cmd_name = unsafe { decode_c_string_lossy(name_ptr) };
        if cmd_name.is_empty() {
            return;
        }

        let mut args = String::new();
        for i in 1..argc {
            let arg_ptr = (backend.argv)(i);
            if !arg_ptr.is_null() {
                let decoded = unsafe { decode_c_string_lossy(arg_ptr) };
                if !args.is_empty() {
                    args.push(' ');
                }
                args.push_str(&decoded);
            }
        }

        crate::host::HostRuntime::with_manager(|manager| {
            if let Some(m) = manager {
                m.dispatch_command(&cmd_name, 0, &args);
            }
        });
    });
}

/// Register server commands pointing at the shared host CLI handler.
pub fn register_host_commands_with_names(
    names: &[&str],
    mut add: impl FnMut(&str, unsafe extern "C" fn()),
) {
    for &name in names {
        add(name, handle_host_command);
    }
}

/// Register default server commands (`goldsrc-rs`, `grs`, `meta-rs`, `mrs`).
pub fn register_host_commands(add: impl FnMut(&str, unsafe extern "C" fn())) {
    register_host_commands_with_names(&["goldsrc-rs", "grs", "meta-rs", "mrs"], add);
}

/// Register all commands currently exposed by loaded plugins as direct server console commands.
pub fn register_plugin_server_commands(mut add: impl FnMut(&str, unsafe extern "C" fn())) {
    crate::host::HostRuntime::with_manager(|manager| {
        if let Some(mgr) = manager {
            for cmd in mgr.registered_commands() {
                add(&cmd, handle_plugin_server_command);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_command_spec() {
        assert!(find_command_spec("plugins").is_some());
        assert!(find_command_spec("pl").is_some());
        assert!(find_command_spec("p").is_none());
        assert!(find_command_spec("ps").is_none());
        assert!(find_command_spec("rld").is_none());
        assert!(find_command_spec("reload").is_none());
        assert!(find_command_spec("sessions").is_some());
        assert!(find_command_spec("sess").is_some());
        assert!(find_command_spec("cvars").is_some());
        assert!(find_command_spec("cv").is_some());
        assert!(find_command_spec("watchers").is_some());
        assert!(find_command_spec("watch").is_none());
        assert!(find_command_spec("w").is_some());
        assert!(find_command_spec("cmd").is_some());
        assert!(find_command_spec("c").is_some());
        assert!(find_command_spec("exec").is_some());
        assert!(find_command_spec("ex").is_some());
        assert!(find_command_spec("status").is_some());
        assert!(find_command_spec("st").is_some());
        assert!(find_command_spec("s").is_none());
        assert!(find_command_spec("stat").is_none());
        assert!(find_command_spec("version").is_some());
        assert!(find_command_spec("ver").is_some());
        assert!(find_command_spec("v").is_none());
        assert!(find_command_spec("hardware").is_some());
        assert!(find_command_spec("hw").is_some());
        assert!(find_command_spec("sysinfo").is_none());
        assert!(find_command_spec("extensions").is_some());
        assert!(find_command_spec("ext").is_some());
        assert!(find_command_spec("e").is_none());
        assert!(find_command_spec("help").is_some());
        assert!(find_command_spec("h").is_some());
        assert!(find_command_spec("?").is_some());
        assert!(find_command_spec("nonexistent").is_none());
        assert!(find_command_spec("foobar_xyz").is_none());
    }

    #[test]
    fn test_print_command_help() {
        let spec = find_command_spec("plugins").unwrap();
        let mut output = String::new();
        print_command_help(spec, |s| output.push_str(s));
        assert!(output.contains("grs plugins"));
        assert!(output.contains("list"));
        assert!(output.contains("--flat"));
        assert!(output.contains("--paused"));

        let watcher_spec = find_command_spec("watchers").unwrap();
        let mut watcher_output = String::new();
        print_command_help(watcher_spec, |s| watcher_output.push_str(s));
        assert!(watcher_output.contains("grs watchers"));
        assert!(watcher_output.contains("list"));
        assert!(watcher_output.contains("pause"));
        assert!(watcher_output.contains("resume"));
    }

    #[test]
    fn test_print_global_help() {
        let mut output = String::new();
        print_host_help(|s| output.push_str(s));
        assert!(output.contains("GoldSrc.rs Management CLI"));
        assert!(output.contains("[plugin:lifecycle]"));
        assert!(output.contains("[watcher:fs]"));
        assert!(output.contains("[exec:dispatch]"));
        assert!(output.contains("[sys:runtime]"));
        assert!(output.contains("[sys:help]"));

        let mut cat_output = String::new();
        let matched = print_category_help("plugin", |s| cat_output.push_str(s));
        assert!(matched);
        assert!(cat_output.contains("plugins"));
        assert!(cat_output.contains("plugin:lifecycle"));
    }

    #[test]
    fn test_dispatch_command_help() {
        let mut output = String::new();
        let args = vec![OsString::from("grs"), OsString::from("--help")];
        dispatch_host_command(args, None, ("0.10.0", "abc", "x86"), |s| output.push_str(s));
        assert!(output.contains("GoldSrc.rs Management CLI"));

        let mut output_cmd = String::new();
        let args_cmd = vec![
            OsString::from("grs"),
            OsString::from("help"),
            OsString::from("plugins"),
        ];
        dispatch_host_command(args_cmd, None, ("0.10.0", "abc", "x86"), |s| {
            output_cmd.push_str(s)
        });
        assert!(output_cmd.contains("grs plugins"));
    }

    #[test]
    fn test_levenshtein_and_command_suggestions() {
        use router::{levenshtein_distance, suggest_command, suggest_subcommand};

        assert_eq!(levenshtein_distance("plugin", "plugins"), 1);
        assert_eq!(levenshtein_distance("plguins", "plugins"), 2);
        assert_eq!(levenshtein_distance("watchr", "watchers"), 2);
        assert_eq!(levenshtein_distance("completely_different", "plugins"), 16);
        assert_eq!(
            levenshtein_distance(
                "this_is_a_very_long_string_that_exceeds_thirty_two_chars",
                "plugins"
            ),
            usize::MAX
        );

        assert_eq!(suggest_command("plugin"), Some("plugins"));
        assert_eq!(suggest_command("plguins"), Some("plugins"));
        assert_eq!(suggest_command("watcher"), Some("watchers"));
        assert_eq!(suggest_command("unknown_xyz"), None);

        let subcmds = &[
            "list", "info", "load", "unload", "reload", "pause", "unpause",
        ];
        assert_eq!(suggest_subcommand("reloadd", subcmds), Some("reload"));
        assert_eq!(suggest_subcommand("paws", subcmds), Some("pause")); // distance is 2
        assert_eq!(suggest_subcommand("completely_unrelated", subcmds), None);

        let mut output = String::new();
        let args_typo = vec![OsString::from("grs"), OsString::from("plguins")];
        dispatch_host_command(args_typo, None, ("0.10.0", "abc", "x86"), |s| {
            output.push_str(s)
        });
        assert!(output.contains("Unknown command 'plguins'. Did you mean 'plugins'?"));
    }

    #[test]
    fn test_shortcuts_and_watchers_dispatch() {
        // Test `grs watchers` dispatch with None manager (should not deadlock or error)
        let mut out_watchers = String::new();
        let args_watchers = vec![OsString::from("grs"), OsString::from("watchers")];
        dispatch_host_command(args_watchers, None, ("0.10.0", "abc", "x86"), |s| {
            out_watchers.push_str(s)
        });
        assert!(out_watchers.contains("Watchers (0)"));

        // Test `grs w` alias
        let mut out_w = String::new();
        let args_w = vec![OsString::from("grs"), OsString::from("w")];
        dispatch_host_command(args_w, None, ("0.10.0", "abc", "x86"), |s| {
            out_w.push_str(s)
        });
        assert!(out_w.contains("Watchers (0)"));

        // Test `grs plugins list` hierarchical command
        let mut out_pl = String::new();
        let args_pl = vec![
            OsString::from("grs"),
            OsString::from("plugins"),
            OsString::from("list"),
        ];
        dispatch_host_command(args_pl, None, ("0.10.0", "abc", "x86"), |s| {
            out_pl.push_str(s)
        });
        assert!(out_pl.contains("WASM Host not initialized.") || out_pl.contains("WASM plugins"));

        // Test `grs pl ps` (plugins alias + ps list subcommand)
        let mut out_pl_ps = String::new();
        let args_pl_ps = vec![
            OsString::from("grs"),
            OsString::from("pl"),
            OsString::from("ps"),
        ];
        dispatch_host_command(args_pl_ps, None, ("0.10.0", "abc", "x86"), |s| {
            out_pl_ps.push_str(s)
        });
        assert!(
            out_pl_ps.contains("WASM Host not initialized.") || out_pl_ps.contains("WASM plugins")
        );

        // Test `grs status`
        let mut out_status = String::new();
        let args_status = vec![OsString::from("grs"), OsString::from("status")];
        dispatch_host_command(args_status, None, ("0.10.0", "abc", "x86"), |s| {
            out_status.push_str(s)
        });
        assert!(out_status.contains("GoldSrc.rs Host Engine Status"));
        assert!(out_status.contains("Watchers:"));

        // Test `grs hardware` & `grs hw`
        let mut out_hw = String::new();
        let args_hw = vec![OsString::from("grs"), OsString::from("hardware")];
        dispatch_host_command(args_hw, None, ("0.10.0", "abc", "x86"), |s| {
            out_hw.push_str(s);
        });
        assert!(out_hw.contains("GoldSrc.rs Host Hardware & Diagnostics"));
        assert!(out_hw.contains("CPU Model"));
        assert!(out_hw.contains("HLDS Process"));

        // Test `grs hardware --json`
        let mut out_hw_json = String::new();
        let args_hw_json = vec![
            OsString::from("grs"),
            OsString::from("hw"),
            OsString::from("--json"),
        ];
        dispatch_host_command(args_hw_json, None, ("0.10.0", "abc", "x86"), |s| {
            out_hw_json.push_str(s);
        });
        assert!(out_hw_json.contains("\"cpu\""));
        assert!(out_hw_json.contains("\"memory\""));
        assert!(out_hw_json.contains("\"performance\""));

        // Test `grs sessions list` & `grs sess list`
        let mut out_sess = String::new();
        let args_sess = vec![
            OsString::from("grs"),
            OsString::from("sess"),
            OsString::from("list"),
        ];
        dispatch_host_command(args_sess, None, ("0.10.0", "abc", "x86"), |s| {
            out_sess.push_str(s);
        });
        assert!(out_sess.contains("Active Client Sessions"));

        // Test `grs cvars list` & `grs cv list`
        let mut out_cv = String::new();
        let args_cv = vec![
            OsString::from("grs"),
            OsString::from("cv"),
            OsString::from("list"),
        ];
        dispatch_host_command(args_cv, None, ("0.10.0", "abc", "x86"), |s| {
            out_cv.push_str(s);
        });
        assert!(out_cv.contains("Host Console Variables"));
    }

    #[test]
    fn test_pipeline_dispatching_chain_execution() {
        let cfg = crate::config::ConsolePipelineConfig::default();
        let plan = ConsolePipelinePreprocessor::parse(
            "grs version && grs cv list",
            CommandDomain::Server,
            &cfg,
        )
        .expect("Valid pipeline syntax");

        let mut output = Vec::new();
        let res = ConsolePipelinePreprocessor::execute_plan(
            &plan,
            CommandDomain::Server,
            |cmd_args, _input| {
                let mut buf = String::new();
                let os_args: Vec<OsString> = cmd_args.iter().map(OsString::from).collect();
                dispatch_host_command(os_args, None, ("0.20.0", "test_git", "x86_64"), |chunk| {
                    buf.push_str(chunk);
                });
                (CommandStatus::Success, buf)
            },
            |line| output.push(line.to_string()),
        );

        assert!(res.is_ok());
        assert_eq!(output.len(), 2);
        assert!(output[0].contains("GoldSrc.rs Host v0.20.0"));
        assert!(output[1].contains("Host Console Variables"));
    }
}
