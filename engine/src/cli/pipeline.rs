//! Console streaming pipeline preprocessor, command chaining, and I/O redirection.
//!
//! Provides parsing and execution semantics for:
//! - Piped commands: `cmd1 | cmd2 | cmd3`
//! - Conditional chaining: `cmd1 && cmd2`
//! - File output redirection: `cmd > file.txt` (truncate) and `cmd >> file.txt` (append)
//! - File input redirection: `cmd < file.txt`
//!
//! Evaluated according to domain policies (`Server`, `ClientConsole`, `Chat`) defined
//! in `goldsrc.toml` under `[console.pipeline]`.

use crate::cli::response::CommandStatus;
use crate::config::pipeline::{ConsolePipelineConfig, PipelineDomainPolicy};
use std::path::{Path, PathBuf};

/// Execution origin domain for an incoming command string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDomain {
    /// Server console (HLDS engine console or RCON). Trusted.
    Server,
    /// Connected player console (`ClientCommand`). Semi-trusted.
    ClientConsole,
    /// In-game chat (`say`, `say_team`). Public / untrusted.
    Chat,
}

/// A parsed redirection destination or source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirection {
    /// Overwrite file (`> target`).
    OutputTruncate(String),
    /// Append to file (`>> target`).
    OutputAppend(String),
    /// Read file into stdin (`< source`).
    Input(String),
}

/// A single atomic command invocation within a pipeline or chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelinedCommand {
    /// Command line arguments (e.g. `["grs", "plugins", "list"]`).
    pub args: Vec<String>,
    /// Optional redirection attached to this specific command.
    pub redirection: Option<Redirection>,
}

/// A chain of commands connected via pipes `|`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeChain {
    /// Sequence of piped commands: output of `commands[i]` flows to input of `commands[i+1]`.
    pub commands: Vec<PipelinedCommand>,
}

/// A full parsed execution plan composed of `&&` conditional chains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    /// Chains connected by `&&`: each chain executes only if the previous chain succeeds.
    pub chains: Vec<PipeChain>,
}

/// Errors produced during command pipeline preprocessing or validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    #[error("Pipeline preprocessor is disabled by configuration")]
    Disabled,

    #[error("Piping ('|') is not permitted in {domain:?}")]
    PipesNotPermitted { domain: CommandDomain },

    #[error("Command chaining ('&&') is not permitted in {domain:?}")]
    ChainingNotPermitted { domain: CommandDomain },

    #[error("File redirection ('>' or '<') is not permitted in {domain:?}")]
    RedirectionNotPermitted { domain: CommandDomain },

    #[error("Pipeline depth {depth} exceeds configured maximum of {max}")]
    DepthExceeded { depth: usize, max: usize },

    #[error("Syntax error: trailing or empty operator in command string")]
    InvalidSyntax,

    #[error("Illegal file path outside VFS sandbox: '{path}'")]
    PathEscapesSandbox { path: String },
}

/// Validates whether a file target is safely constrained within the server sandbox.
///
/// Prevents directory traversal (`..`, absolute drive roots `C:\`, `/etc`, etc.)
pub fn sanitize_vfs_redirection_path(raw: &str) -> Result<PathBuf, PipelineError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PipelineError::InvalidSyntax);
    }

    let p = Path::new(trimmed);
    if p.is_absolute() {
        return Err(PipelineError::PathEscapesSandbox {
            path: trimmed.to_string(),
        });
    }

    // Explicit check for Windows drive prefixes (e.g. "C:\" or "C:") even when compiled on Unix
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(PipelineError::PathEscapesSandbox {
            path: trimmed.to_string(),
        });
    }

    for component in p.components() {
        match component {
            std::path::Component::ParentDir => {
                return Err(PipelineError::PathEscapesSandbox {
                    path: trimmed.to_string(),
                });
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(PipelineError::PathEscapesSandbox {
                    path: trimmed.to_string(),
                });
            }
            _ => {}
        }
    }

    Ok(PathBuf::from(trimmed))
}

/// Resolves a sanitized target path for writing redirected output (`>` / `>>`).
///
/// Places files into the framework root (`cstrike/goldsrc` or `cstrike/addons/goldsrc`)
/// so that sandboxed WASM plugins (`cat`, `grep`, `ls`) can seamlessly see and access them.
pub fn resolve_redirection_write_path(raw: &str) -> Result<PathBuf, PipelineError> {
    let rel = sanitize_vfs_redirection_path(raw)?;
    let backend = crate::host::HostRuntime::backend_type();
    let fw_dir = crate::paths::PathResolver::framework_dir(backend);
    if fw_dir.exists() {
        Ok(fw_dir.join(rel))
    } else {
        Ok(rel)
    }
}

/// Resolves a sanitized source path for reading redirected input (`<`).
///
/// Checks framework directory first, then fallback to current working directory.
pub fn resolve_redirection_read_path(raw: &str) -> Result<PathBuf, PipelineError> {
    let rel = sanitize_vfs_redirection_path(raw)?;
    let backend = crate::host::HostRuntime::backend_type();
    let fw_dir = crate::paths::PathResolver::framework_dir(backend);
    let in_fw = fw_dir.join(&rel);
    if in_fw.exists() { Ok(in_fw) } else { Ok(rel) }
}

/// Splits a raw command line string into tokens respecting single/double quotes.
pub fn tokenize_command_line(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = None;
    for c in input.chars() {
        match c {
            '"' | '\'' => {
                if in_quotes == Some(c) {
                    in_quotes = None;
                } else if in_quotes.is_none() {
                    in_quotes = Some(c);
                } else {
                    current.push(c);
                }
            }
            ' ' | '\t' if in_quotes.is_none() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(c);
            }
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Console Pipeline Preprocessor that parses and validates command lines against domain policies.
pub struct ConsolePipelinePreprocessor;

impl ConsolePipelinePreprocessor {
    /// Returns the active policy for the given domain.
    pub fn domain_policy(
        config: &ConsolePipelineConfig,
        domain: CommandDomain,
    ) -> PipelineDomainPolicy {
        match domain {
            CommandDomain::Server => config.server,
            CommandDomain::ClientConsole => config.client_console,
            CommandDomain::Chat => config.chat,
        }
    }

    /// Checks if a raw command string contains any pipeline operators (`|`, `&&`, `>`, `<`).
    pub fn has_pipeline_operators(input: &str) -> bool {
        let mut in_quotes = None;
        let mut chars = input.chars().peekable();

        while let Some(c) = chars.next() {
            match c {
                '"' | '\'' => {
                    if in_quotes == Some(c) {
                        in_quotes = None;
                    } else if in_quotes.is_none() {
                        in_quotes = Some(c);
                    }
                }
                '|' if in_quotes.is_none() => return true,
                '&' if in_quotes.is_none() => {
                    if chars.peek() == Some(&'&') {
                        return true;
                    }
                }
                '>' | '<' if in_quotes.is_none() => return true,
                _ => {}
            }
        }
        false
    }

    /// Parses a raw command string into an `ExecutionPlan`, validating permissions against `domain`.
    pub fn parse(
        input: &str,
        domain: CommandDomain,
        config: &ConsolePipelineConfig,
    ) -> Result<ExecutionPlan, PipelineError> {
        if !config.enabled {
            return Err(PipelineError::Disabled);
        }

        let policy = Self::domain_policy(config, domain);

        // Step 1: Split into `&&` chains
        let raw_chains = Self::split_by_operator(input, "&&");
        if raw_chains.len() > 1 && !policy.chaining {
            return Err(PipelineError::ChainingNotPermitted { domain });
        }

        let mut chains = Vec::with_capacity(raw_chains.len());

        for chain_str in raw_chains {
            let chain_trimmed = chain_str.trim();
            if chain_trimmed.is_empty() {
                return Err(PipelineError::InvalidSyntax);
            }

            // Step 2: Split each chain into piped segments `|`
            let raw_pipes = Self::split_by_operator(chain_trimmed, "|");
            if raw_pipes.len() > 1 && !policy.pipes {
                return Err(PipelineError::PipesNotPermitted { domain });
            }

            if raw_pipes.len() > config.max_depth {
                return Err(PipelineError::DepthExceeded {
                    depth: raw_pipes.len(),
                    max: config.max_depth,
                });
            }

            let mut commands = Vec::with_capacity(raw_pipes.len());

            for pipe_str in raw_pipes {
                let cmd = Self::parse_single_command(pipe_str.trim(), &policy, domain)?;
                commands.push(cmd);
            }

            chains.push(PipeChain { commands });
        }

        Ok(ExecutionPlan { chains })
    }

    /// Parses a single command segment, extracting optional redirections (`>`, `>>`, `<`).
    fn parse_single_command(
        segment: &str,
        policy: &PipelineDomainPolicy,
        domain: CommandDomain,
    ) -> Result<PipelinedCommand, PipelineError> {
        if segment.is_empty() {
            return Err(PipelineError::InvalidSyntax);
        }

        let tokens = tokenize_command_line(segment);
        if tokens.is_empty() {
            return Err(PipelineError::InvalidSyntax);
        }

        let mut args = Vec::new();
        let mut redirection = None;
        let mut iter = tokens.into_iter();

        while let Some(tok) = iter.next() {
            match tok.as_str() {
                ">" | ">>" => {
                    if !policy.redirection {
                        return Err(PipelineError::RedirectionNotPermitted { domain });
                    }
                    let target = iter.next().ok_or(PipelineError::InvalidSyntax)?;
                    let _ = sanitize_vfs_redirection_path(&target)?;
                    redirection = if tok == ">" {
                        Some(Redirection::OutputTruncate(target))
                    } else {
                        Some(Redirection::OutputAppend(target))
                    };
                }
                "<" => {
                    if !policy.input_redirection {
                        return Err(PipelineError::RedirectionNotPermitted { domain });
                    }
                    let source = iter.next().ok_or(PipelineError::InvalidSyntax)?;
                    let _ = sanitize_vfs_redirection_path(&source)?;
                    redirection = Some(Redirection::Input(source));
                }
                _ => {
                    args.push(tok);
                }
            }
        }

        if args.is_empty() {
            return Err(PipelineError::InvalidSyntax);
        }

        Ok(PipelinedCommand { args, redirection })
    }

    /// Splits a string by an operator (`&&` or `|`) while ignoring occurrences inside quotes.
    fn split_by_operator(input: &str, op: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut current = String::new();
        let mut in_quotes = None;
        let chars: Vec<char> = input.chars().collect();
        let mut i = 0;
        let op_chars: Vec<char> = op.chars().collect();
        let op_len = op_chars.len();

        while i < chars.len() {
            let c = chars[i];
            if c == '"' || c == '\'' {
                if in_quotes == Some(c) {
                    in_quotes = None;
                } else if in_quotes.is_none() {
                    in_quotes = Some(c);
                }
                current.push(c);
                i += 1;
                continue;
            }

            if in_quotes.is_none()
                && i + op_len <= chars.len()
                && chars[i..i + op_len] == op_chars[..]
            {
                result.push(std::mem::take(&mut current));
                i += op_len;
                continue;
            }

            current.push(c);
            i += 1;
        }

        result.push(current);
        result
    }

    /// Executes an execution plan, connecting stdout of each piped command into stdin of the next.
    ///
    /// Evaluates `&&` chains conditionally (aborts early on failure).
    pub fn execute_plan<FExec, FPrint>(
        plan: &ExecutionPlan,
        _domain: CommandDomain,
        mut exec_cmd: FExec,
        mut print: FPrint,
    ) -> Result<(), PipelineError>
    where
        FExec: FnMut(&[String], Option<&str>) -> (CommandStatus, String),
        FPrint: FnMut(&str),
    {
        for chain in &plan.chains {
            let mut piped_input: Option<String> = None;

            for (idx, cmd) in chain.commands.iter().enumerate() {
                // Handle input redirection `< file` on the first command if present
                let mut effective_input = piped_input.take();
                if idx == 0
                    && let Some(Redirection::Input(ref source)) = cmd.redirection
                {
                    let path = resolve_redirection_read_path(source)?;
                    match std::fs::read_to_string(&path) {
                        Ok(content) => effective_input = Some(content),
                        Err(e) => {
                            print(&format!("[GoldSrc.rs] Failed to read '< {source}': {e}\n"));
                            return Err(PipelineError::InvalidSyntax);
                        }
                    }
                }

                let (status, stdout) = exec_cmd(&cmd.args, effective_input.as_deref());

                let is_last = idx == chain.commands.len() - 1;

                if is_last {
                    // Check output redirection `>` or `>>`
                    match &cmd.redirection {
                        Some(Redirection::OutputTruncate(target)) => {
                            let path = resolve_redirection_write_path(target)?;
                            if let Err(e) = std::fs::write(&path, &stdout) {
                                print(&format!("[GoldSrc.rs] Failed to write '> {target}': {e}\n"));
                            }
                        }
                        Some(Redirection::OutputAppend(target)) => {
                            let path = resolve_redirection_write_path(target)?;
                            use std::io::Write;
                            if let Ok(mut file) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&path)
                            {
                                let _ = file.write_all(stdout.as_bytes());
                            } else {
                                print(&format!("[GoldSrc.rs] Failed to append '>> {target}'\n"));
                            }
                        }
                        _ => {
                            if !stdout.is_empty() {
                                print(&stdout);
                            }
                        }
                    }

                    // For `&&` chains: if status is not Success or Notice, abort subsequent chains
                    if !matches!(status, CommandStatus::Success | CommandStatus::Notice) {
                        return Ok(());
                    }
                } else {
                    // Forward output to next piped command
                    piped_input = Some(stdout);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_pipeline_operators() {
        assert!(ConsolePipelinePreprocessor::has_pipeline_operators(
            "grs pl list | grep test"
        ));
        assert!(ConsolePipelinePreprocessor::has_pipeline_operators(
            "status && ping"
        ));
        assert!(ConsolePipelinePreprocessor::has_pipeline_operators(
            "grs pl list > plugins.txt"
        ));
        assert!(ConsolePipelinePreprocessor::has_pipeline_operators(
            "cmd < in.txt"
        ));

        // In quotes should be ignored
        assert!(!ConsolePipelinePreprocessor::has_pipeline_operators(
            "say \"hello | world\""
        ));
        assert!(!ConsolePipelinePreprocessor::has_pipeline_operators(
            "say 'admin && player'"
        ));
        assert!(!ConsolePipelinePreprocessor::has_pipeline_operators(
            "normal_command 123"
        ));
    }

    #[test]
    fn test_parse_server_pipe_and_chaining() {
        let cfg = ConsolePipelineConfig::default();
        let input = "grs pl list | grep auth && echo done > out.txt";
        let plan = ConsolePipelinePreprocessor::parse(input, CommandDomain::Server, &cfg).unwrap();

        assert_eq!(plan.chains.len(), 2);
        // Chain 1: grs pl list | grep auth
        assert_eq!(plan.chains[0].commands.len(), 2);
        assert_eq!(plan.chains[0].commands[0].args, vec!["grs", "pl", "list"]);
        assert_eq!(plan.chains[0].commands[1].args, vec!["grep", "auth"]);

        // Chain 2: echo done > out.txt
        assert_eq!(plan.chains[1].commands.len(), 1);
        assert_eq!(plan.chains[1].commands[0].args, vec!["echo", "done"]);
        assert_eq!(
            plan.chains[1].commands[0].redirection,
            Some(Redirection::OutputTruncate("out.txt".to_string()))
        );
    }

    #[test]
    fn test_client_console_policy_blocks_pipes_allows_chaining() {
        let cfg = ConsolePipelineConfig::default();

        // Chaining is allowed for client binds
        let valid_bind = "slot1 && +attack";
        let plan =
            ConsolePipelinePreprocessor::parse(valid_bind, CommandDomain::ClientConsole, &cfg)
                .unwrap();
        assert_eq!(plan.chains.len(), 2);

        // Pipe is forbidden for clients
        let pipe_attempt = "status | dump";
        let err =
            ConsolePipelinePreprocessor::parse(pipe_attempt, CommandDomain::ClientConsole, &cfg)
                .unwrap_err();
        assert_eq!(
            err,
            PipelineError::PipesNotPermitted {
                domain: CommandDomain::ClientConsole
            }
        );

        // Redirection is forbidden for clients
        let redir_attempt = "status > test.txt";
        let err2 =
            ConsolePipelinePreprocessor::parse(redir_attempt, CommandDomain::ClientConsole, &cfg)
                .unwrap_err();
        assert_eq!(
            err2,
            PipelineError::RedirectionNotPermitted {
                domain: CommandDomain::ClientConsole
            }
        );
    }

    #[test]
    fn test_chat_domain_policy_blocks_all_by_default() {
        let cfg = ConsolePipelineConfig::default();

        let pipe_attempt = "!menu | !stats";
        let err = ConsolePipelinePreprocessor::parse(pipe_attempt, CommandDomain::Chat, &cfg)
            .unwrap_err();
        assert_eq!(
            err,
            PipelineError::PipesNotPermitted {
                domain: CommandDomain::Chat
            }
        );

        let chain_attempt = "!menu && !buy";
        let err2 = ConsolePipelinePreprocessor::parse(chain_attempt, CommandDomain::Chat, &cfg)
            .unwrap_err();
        assert_eq!(
            err2,
            PipelineError::ChainingNotPermitted {
                domain: CommandDomain::Chat
            }
        );
    }

    #[test]
    fn test_vfs_sandbox_escapes_prevented() {
        assert!(sanitize_vfs_redirection_path("logs/test.txt").is_ok());
        assert!(sanitize_vfs_redirection_path("test.txt").is_ok());

        assert!(matches!(
            sanitize_vfs_redirection_path("../etc/passwd"),
            Err(PipelineError::PathEscapesSandbox { .. })
        ));
        assert!(matches!(
            sanitize_vfs_redirection_path("/absolute/path"),
            Err(PipelineError::PathEscapesSandbox { .. })
        ));
        assert!(matches!(
            sanitize_vfs_redirection_path("C:\\Windows\\System32"),
            Err(PipelineError::PathEscapesSandbox { .. })
        ));
    }

    #[test]
    fn test_max_depth_exceeded() {
        let cfg = ConsolePipelineConfig {
            max_depth: 2,
            ..Default::default()
        };

        let input = "cmd1 | cmd2 | cmd3";
        let err =
            ConsolePipelinePreprocessor::parse(input, CommandDomain::Server, &cfg).unwrap_err();
        assert_eq!(err, PipelineError::DepthExceeded { depth: 3, max: 2 });
    }

    #[test]
    fn test_execute_plan_piping_and_chain_abort() {
        let cfg = ConsolePipelineConfig::default();
        let plan = ConsolePipelinePreprocessor::parse(
            "echo Hello | tr a-z A-Z && fail_cmd && echo ShouldNotRun",
            CommandDomain::Server,
            &cfg,
        )
        .unwrap();

        let mut executed = Vec::new();
        let mut printed = Vec::new();

        let res = ConsolePipelinePreprocessor::execute_plan(
            &plan,
            CommandDomain::Server,
            |args, input| {
                executed.push((args.to_vec(), input.map(|s| s.to_string())));
                let cmd_name = &args[0];
                match cmd_name.as_str() {
                    "echo" => (CommandStatus::Success, args[1..].join(" ")),
                    "tr" => {
                        let in_str = input.unwrap_or_default();
                        (CommandStatus::Success, in_str.to_uppercase())
                    }
                    "fail_cmd" => (CommandStatus::Error, "Command failed".to_string()),
                    _ => (CommandStatus::Success, String::new()),
                }
            },
            |out| printed.push(out.to_string()),
        );

        assert!(res.is_ok());
        // First chain has 2 commands (echo | tr)
        assert_eq!(executed.len(), 3);
        assert_eq!(executed[0].0, vec!["echo", "Hello"]);
        assert_eq!(executed[0].1, None);

        assert_eq!(executed[1].0, vec!["tr", "a-z", "A-Z"]);
        assert_eq!(executed[1].1, Some("Hello".to_string()));

        // Second chain has fail_cmd, which returned Error, so ShouldNotRun was aborted!
        assert_eq!(executed[2].0, vec!["fail_cmd"]);
        assert_eq!(printed, vec!["HELLO", "Command failed"]);
    }
}
