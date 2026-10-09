use clap::{Parser, Subcommand};
use compact_str::CompactString;

use crate::config;
use crate::config::types::EditSystem;

#[derive(Parser, Debug, Default, Clone)]
#[command(name = "zerostack", version, about = "Minimal coding agent")]
pub struct Cli {
    #[arg(short = 'p', long = "print", help = "Print response and exit")]
    pub print: bool,

    #[arg(
        long = "pure-stdout",
        help = "With -p: also print tool calls/results to stdout"
    )]
    pub pure_stdout: bool,

    #[arg(long = "print-config", help = "Print resolved configuration and exit")]
    pub print_config: bool,

    #[arg(
        long = "emacs",
        help = "Run the native Emacs Unix socket protocol for this session"
    )]
    pub emacs: bool,

    #[cfg(unix)]
    #[arg(long, hide = true)]
    pub emacs_launch: bool,

    #[arg(
        long = "emacs-list",
        help = "List running native Emacs protocol sessions"
    )]
    pub emacs_list: bool,

    #[arg(long, hide = true)]
    pub emacs_archive: Option<std::path::PathBuf>,

    #[arg(long, hide = true, default_value = "100")]
    pub emacs_archive_cols: usize,

    #[arg(
        long = "emacs-board",
        help = "Print an Emacs-readable project/worktree/session board snapshot"
    )]
    pub emacs_board: bool,

    #[arg(long = "emacs-board-search", hide = true, value_name = "QUERY")]
    pub emacs_board_search: Option<String>,

    #[arg(
        long = "emacs-dismiss-attention",
        help = "Dismiss a session from the Emacs board Needs attention section"
    )]
    pub emacs_dismiss_attention: Option<String>,

    #[arg(long = "emacs-delete-session", hide = true)]
    pub emacs_delete_session: Option<String>,

    #[arg(short = 'c', long = "continue", help = "Continue most recent session")]
    pub continue_session: bool,

    #[arg(short = 'r', long = "resume", help = "List recent sessions")]
    pub resume: bool,

    #[arg(long = "session", help = "Load session by ID prefix")]
    pub session: Option<String>,

    #[arg(long = "no-session", help = "Ephemeral mode, do not save")]
    pub no_session: bool,

    #[arg(long, hide = true)]
    pub subagent_session_id: Option<String>,

    #[arg(long, hide = true)]
    pub subagent_parent_session: Option<String>,

    #[arg(long, hide = true, value_parser = ["read", "write"])]
    pub subagent_access: Option<String>,

    #[arg(long, hide = true)]
    pub subagent_live: bool,

    #[arg(long, hide = true)]
    pub subagent_finalize_at_unix_ms: Option<u64>,

    #[arg(long = "provider", env = "ZS_PROVIDER", help = "API provider")]
    pub provider: Option<String>,

    #[arg(long = "model", env = "ZS_MODEL", help = "Model name")]
    pub model: Option<String>,

    #[arg(long = "quick-model", help = "Use a named quick model from config")]
    pub quick_model: Option<String>,

    #[arg(
        long = "api-key",
        help = "API key for the provider (WARNING: visible to other users via ps/htop; prefer env vars)"
    )]
    pub api_key: Option<String>,

    #[arg(long = "max-tokens", help = "Maximum tokens in response")]
    pub max_tokens: Option<u64>,

    #[arg(long = "max-agent-turns", help = "Maximum agent turns")]
    pub max_agent_turns: Option<usize>,

    #[arg(long = "temperature", help = "Model temperature (0.0 to 2.0)")]
    pub temperature: Option<f64>,

    #[arg(
        long = "reasoning-effort",
        value_parser = ["off", "none", "minimal", "low", "medium", "high", "xhigh", "max"],
        help = "OpenAI reasoning effort for supported models (off/none, low, medium, high, xhigh, max)"
    )]
    pub reasoning_effort: Option<String>,

    #[arg(short = 't', long = "tools", help = "Allowlist specific tools")]
    pub tools: Vec<String>,

    #[arg(long = "no-tools", help = "Disable all tools")]
    pub no_tools: bool,

    #[arg(long = "no-color", help = "Disable colored TUI output")]
    pub no_color: bool,

    #[arg(long = "restrictive", short = 'R', help = "Ask for all operations")]
    pub restrictive: bool,

    #[arg(long = "read-only", help = "Allow reads only, deny everything else")]
    pub read_only: bool,

    #[arg(long = "guarded", help = "Allow reads, ask for all other operations")]
    pub guarded: bool,

    #[arg(
        long = "accept-all",
        help = "Auto-accept all operations within the working directory"
    )]
    pub accept_all: bool,

    #[arg(
        long = "yolo",
        help = "Allow all operations except destructive bash commands"
    )]
    pub yolo: bool,

    #[arg(
        long = "dangerously-skip-permissions",
        help = "Skip all permission checks (allow everything without any guard)"
    )]
    pub dangerously_skip_permissions: bool,

    #[arg(
        long = "sandbox",
        help = "Run bash commands inside bubblewrap (bwrap) sandbox"
    )]
    pub sandbox: bool,

    #[arg(
        long = "sandbox-backend",
        help = "Sandbox backend: bwrap (default) or zerobox"
    )]
    pub sandbox_backend: Option<String>,

    #[arg(
        long = "shell",
        help = "Shell binary to use for bash tool (default: bash)"
    )]
    pub shell: Option<String>,

    #[arg(
        long = "edit-system",
        help = "Edit system (similarity or hashedit). Default: similarity"
    )]
    pub edit_system: Option<String>,

    #[arg(
        long = "no-context-files",
        short = 'n',
        help = "Disable AGENTS.md and ARCHITECTURE.md loading"
    )]
    pub no_context_files: bool,

    #[cfg(feature = "loop")]
    #[arg(
        long = "loop",
        help = "Run in headless loop mode (requires --loop-prompt or message)"
    )]
    pub loop_mode: bool,

    #[cfg(feature = "acp")]
    #[arg(
        long = "acp",
        help = "Enable ACP (Agent Communication Protocol) support"
    )]
    pub acp_enabled: bool,

    #[cfg(feature = "acp")]
    #[arg(long = "acp-host", help = "ACP TCP bind host [default: stdio mode]")]
    pub acp_host: Option<String>,

    #[cfg(feature = "acp")]
    #[arg(long = "acp-port", help = "ACP TCP bind port [default: 7243]")]
    pub acp_port: Option<u16>,

    #[cfg(feature = "loop")]
    #[arg(long = "loop-prompt", help = "Prompt for each loop iteration")]
    pub loop_prompt: Option<String>,

    #[cfg(feature = "loop")]
    #[arg(long = "loop-plan", help = "Plan file path [default: LOOP_PLAN.md]")]
    pub loop_plan: Option<std::path::PathBuf>,

    #[cfg(feature = "loop")]
    #[arg(long = "loop-max", help = "Maximum number of iterations")]
    pub loop_max: Option<u32>,

    #[cfg(feature = "loop")]
    #[arg(
        long = "loop-run",
        help = "Validation command to run after each iteration"
    )]
    pub loop_run: Option<String>,

    #[cfg(feature = "git-worktree")]
    #[arg(long = "worktree", help = "Create a git worktree and cd into it")]
    pub worktree: Option<String>,

    #[cfg(feature = "git-worktree")]
    #[arg(long = "wt-auto-merge", help = "Auto-merge worktree branch on exit")]
    pub wt_auto_merge: bool,

    #[cfg(feature = "git-worktree")]
    #[arg(
        long = "parallel",
        help = "Create a worktree with timestamp name and auto-merge on exit"
    )]
    pub parallel: bool,

    #[cfg(feature = "git-worktree")]
    #[arg(
        long = "wt-base-dir",
        help = "Base directory for worktrees (default: parent of current repo)"
    )]
    pub wt_base_dir: Option<String>,

    #[cfg(feature = "git-worktree")]
    #[arg(
        long = "wt-force",
        help = "Force worktree remove and branch delete even if dirty"
    )]
    pub wt_force: bool,

    #[cfg(feature = "advisor")]
    #[arg(
        long = "advisor",
        help = "Enable advisor tool (model can consult a stronger reviewer model)"
    )]
    pub advisor: bool,

    #[cfg(feature = "advisor")]
    #[arg(
        long = "advisor-model",
        help = "Advisor model name (e.g. 'claude-opus-4-8')"
    )]
    pub advisor_model: Option<String>,

    #[cfg(feature = "advisor")]
    #[arg(
        long = "advisor-max-uses",
        help = "Maximum advisor calls per request (default: 3)"
    )]
    pub advisor_max_uses: Option<usize>,

    #[cfg(feature = "advisor")]
    #[arg(
        long = "advisor-human-handoff",
        help = "Route advisor calls to the user instead of a model",
        default_missing_value = "true",
        num_args = 0..=1,
        require_equals = true,
        default_value = "false"
    )]
    pub advisor_human_handoff: Option<bool>,

    #[cfg(feature = "advisor")]
    #[arg(
        long = "advisor-kilobytes-limit",
        help = "Max total kilobytes of conversation context to send to the advisor (head: half, tail: half). Default: 256",
        default_value = "256"
    )]
    pub advisor_kilobytes_limit: u32,

    #[cfg(feature = "status-signals")]
    #[arg(
        long = "status-socket",
        help = "Unix socket path for status signals (start/stop messages)"
    )]
    pub status_socket: Option<String>,

    #[arg(help = "Prompt message(s)")]
    pub message: Vec<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    #[command(about = "Inspect the project/workspace/session board as JSON")]
    Board {
        #[command(subcommand)]
        command: BoardCommand,
    },
    #[cfg(unix)]
    #[command(about = "Start or prompt persistent background sessions")]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    #[cfg(feature = "git-worktree")]
    #[command(about = "Manage workspaces without a frontend")]
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    #[command(about = "Manage stored provider authentication")]
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    #[command(about = "Inspect or update configuration defaults")]
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum BoardCommand {
    #[command(about = "List board metadata as JSON, optionally filtered or summarized")]
    List(BoardListArgs),
}

#[derive(clap::Args, Debug, Clone, Default)]
pub struct BoardListArgs {
    #[arg(long, help = "Repository root or any of its worktrees")]
    pub repo: Option<std::path::PathBuf>,
    #[arg(long, help = "Exact worktree or loose workspace path")]
    pub path: Option<std::path::PathBuf>,
    #[arg(long, help = "Session UUID or prefix; retains all matches")]
    pub session: Option<String>,
    #[arg(long, help = "Retain only live sessions")]
    pub alive: bool,
    #[arg(
        long,
        help = "Return counts and workspace metadata without session records"
    )]
    pub summary: bool,
}

#[cfg(unix)]
#[derive(Subcommand, Debug, Clone)]
pub enum SessionCommand {
    Start {
        #[arg(long)]
        path: std::path::PathBuf,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
    },
    Send {
        #[arg(long)]
        session: String,
        #[arg(long)]
        prompt: String,
    },
}

#[cfg(feature = "git-worktree")]
#[derive(Subcommand, Debug, Clone)]
pub enum WorkspaceCommand {
    #[cfg(unix)]
    #[command(about = "Wait for readiness, reporting progress on stderr and final JSON on stdout")]
    Wait {
        #[arg(long)]
        job: String,
        #[arg(
            long,
            default_value_t = 600,
            help = "Maximum wait seconds; timeout does not cancel setup"
        )]
        timeout: u64,
    },
    #[cfg(unix)]
    Status {
        #[arg(long)]
        job: String,
    },
    #[cfg(unix)]
    Logs {
        #[arg(long)]
        job: String,
    },
    #[cfg(unix)]
    #[command(hide = true)]
    RunJob {
        #[arg(long)]
        job: String,
    },
    Create {
        #[arg(long)]
        repo: std::path::PathBuf,
        #[arg(long)]
        branch: String,
        #[arg(long)]
        path: std::path::PathBuf,
        #[arg(long, default_value = "origin/HEAD")]
        base: String,
        #[arg(long)]
        description: Option<String>,
        #[cfg(unix)]
        #[arg(
            long,
            help = "Wait for successful setup, then start a persistent session"
        )]
        start_session: bool,
        #[cfg(unix)]
        #[arg(long, requires = "start_session")]
        prompt: Option<String>,
        #[cfg(unix)]
        #[arg(long, requires = "start_session")]
        provider: Option<String>,
        #[cfg(unix)]
        #[arg(long, requires = "start_session")]
        model: Option<String>,
        #[cfg(unix)]
        #[arg(
            long,
            requires = "start_session",
            help = "Maximum setup wait seconds (default: 600); does not cancel setup"
        )]
        timeout: Option<u64>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ConfigCommand {
    #[command(about = "List configured provider names")]
    Providers,
    #[command(about = "List known model ids for a provider")]
    Models {
        #[arg(help = "Provider name; defaults to the configured provider")]
        provider: Option<String>,
    },
    #[command(about = "List configured quick-model names")]
    QuickModels,
    #[command(about = "Persist the default provider and reset model to its default")]
    SetProvider {
        #[arg(help = "Provider name")]
        provider: String,
    },
    #[command(about = "Persist the default model for the current default provider")]
    SetModel {
        #[arg(help = "Model id")]
        model: String,
    },
    #[cfg(feature = "subagents")]
    #[command(about = "Enable or disable subagents by default")]
    SetSubagents {
        #[arg(
            help = "true to enable subagents, false to disable them",
            action = clap::ArgAction::Set
        )]
        enabled: bool,
    },
    #[cfg(feature = "subagents")]
    #[command(about = "Persist the default subagent provider and reset model to its default")]
    SetSubagentProvider {
        #[arg(help = "Provider name")]
        provider: String,
    },
    #[cfg(feature = "subagents")]
    #[command(about = "Persist the default subagent model for the current subagent provider")]
    SetSubagentModel {
        #[arg(help = "Model id")]
        model: String,
    },
    #[cfg(feature = "subagents")]
    #[command(about = "Persist the permissible subagent model names")]
    SetSubagentModels {
        #[arg(help = "One or more model ids or quick-model names", required = true)]
        models: Vec<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum AuthCommand {
    #[command(about = "Log in to a provider")]
    Login {
        #[arg(help = "Provider to log in to: codex or openai-codex")]
        provider: String,
        #[arg(long = "device", help = "Use the device-code login flow")]
        device: bool,
    },
    #[command(about = "Log out from a provider")]
    Logout {
        #[arg(help = "Provider to log out from: codex or openai-codex")]
        provider: String,
    },
    #[command(about = "Show stored authentication status")]
    Status {
        #[arg(help = "Optional provider to show: codex or openai-codex")]
        provider: Option<String>,
    },
    #[command(about = "Store an API key for a provider")]
    SetKey {
        #[arg(
            help = "Provider name, for example openai, anthropic, openrouter, or a custom provider"
        )]
        provider: String,
        #[arg(help = "API key to store")]
        key: String,
    },
    #[command(about = "Remove a stored API key for a provider")]
    UnsetKey {
        #[arg(
            help = "Provider name, for example openai, anthropic, openrouter, or a custom provider"
        )]
        provider: String,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command, ConfigCommand};

    #[test]
    fn parses_board_list() {
        assert!(matches!(
            Cli::try_parse_from(["zerostack", "board", "list"])
                .unwrap()
                .command,
            Some(Command::Board {
                command: super::BoardCommand::List(_)
            })
        ));
    }

    #[test]
    fn parses_board_filters_and_summary() {
        let cli = Cli::try_parse_from([
            "zerostack",
            "board",
            "list",
            "--repo",
            "/repo",
            "--path",
            "/workspace",
            "--session",
            "abc",
            "--alive",
            "--summary",
        ])
        .unwrap();
        let Some(Command::Board {
            command: super::BoardCommand::List(filters),
        }) = cli.command
        else {
            panic!("expected board list")
        };
        assert_eq!(filters.repo.as_deref(), Some(std::path::Path::new("/repo")));
        assert_eq!(
            filters.path.as_deref(),
            Some(std::path::Path::new("/workspace"))
        );
        assert_eq!(filters.session.as_deref(), Some("abc"));
        assert!(filters.alive && filters.summary);
        assert!(Cli::try_parse_from(["zerostack", "board", "list", "--repo"]).is_err());
    }

    #[cfg(all(unix, feature = "git-worktree"))]
    #[test]
    fn parses_workspace_session_chaining_and_requires_explicit_start() {
        let base = [
            "zerostack",
            "workspace",
            "create",
            "--repo",
            "/repo",
            "--branch",
            "task",
            "--path",
            "/workspace",
        ];
        let cli = Cli::try_parse_from(base.into_iter().chain([
            "--start-session",
            "--prompt",
            "Task",
            "--provider",
            "openai",
            "--model",
            "model",
            "--timeout",
            "60",
        ]))
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Workspace {
                command: super::WorkspaceCommand::Create {
                    start_session: true,
                    prompt: Some(_),
                    timeout: Some(60),
                    ..
                }
            })
        ));
        for flag in ["--prompt", "--provider", "--model", "--timeout"] {
            assert!(Cli::try_parse_from(base.into_iter().chain([flag, "1"])).is_err());
        }
        assert!(
            Cli::try_parse_from(
                base.into_iter()
                    .chain(["--start-session", "--timeout", "-1"])
            )
            .is_err()
        );
    }

    #[cfg(all(unix, feature = "git-worktree"))]
    #[test]
    fn parses_workspace_wait_and_validates_timeout() {
        let parse = |timeout| {
            Cli::try_parse_from([
                "zerostack",
                "workspace",
                "wait",
                "--job",
                "id",
                "--timeout",
                timeout,
            ])
        };
        assert!(matches!(
            parse("30").unwrap().command,
            Some(Command::Workspace {
                command: super::WorkspaceCommand::Wait { timeout: 30, .. }
            })
        ));
        assert!(parse("-1").is_err());
        assert!(parse("not-a-number").is_err());
        assert!(Cli::try_parse_from(["zerostack", "workspace", "wait"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn parses_session_control() {
        let cli = Cli::try_parse_from([
            "zerostack",
            "session",
            "start",
            "--path",
            "/tmp",
            "--prompt",
            "Task",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: super::SessionCommand::Start {
                    prompt: Some(_),
                    ..
                }
            })
        ));
        assert!(Cli::try_parse_from(["zerostack", "session", "send", "--session", "id"]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn parses_shared_launcher_and_resumed_session_start() {
        let cli = Cli::try_parse_from([
            "zerostack",
            "--emacs-launch",
            "--session",
            "saved",
            "--no-tools",
        ])
        .unwrap();
        assert!(cli.emacs_launch);
        assert_eq!(cli.session.as_deref(), Some("saved"));
        assert!(cli.no_tools);
        let cli = Cli::try_parse_from([
            "zerostack",
            "session",
            "start",
            "--path",
            "/tmp",
            "--session",
            "saved",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Session {
                command: super::SessionCommand::Start {
                    session: Some(_),
                    ..
                }
            })
        ));
    }

    #[cfg(feature = "git-worktree")]
    #[test]
    fn parses_workspace_default_base() {
        let cli = Cli::try_parse_from([
            "zerostack",
            "workspace",
            "create",
            "--repo",
            "/repo",
            "--branch",
            "task",
            "--path",
            "/workspace",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Command::Workspace {
                command: super::WorkspaceCommand::Create { base, .. },
            } => assert_eq!(base, "origin/HEAD"),
            _ => panic!("expected workspace creation"),
        }
    }

    #[cfg(feature = "subagents")]
    #[test]
    fn parses_set_subagents_boolean_value() {
        let cli = Cli::try_parse_from(["zerostack", "config", "set-subagents", "false"])
            .expect("set-subagents false should parse");
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                command: ConfigCommand::SetSubagents { enabled: false }
            })
        ));
    }
}

impl Cli {
    pub fn resolve_quick_model<'a>(
        &self,
        cfg: &'a config::Config,
    ) -> Option<&'a config::QuickModelConfig> {
        let name = self.quick_model.as_deref()?;
        cfg.quick_models.as_ref().and_then(|m| m.get(name))
    }

    pub fn resolve_model(&self, cfg: &config::Config) -> CompactString {
        if let Some(m) = self.model.as_deref() {
            return CompactString::new(m);
        }
        if self.provider.is_some()
            && let Some((model, _)) =
                crate::provider::default_model_for_provider(&self.resolve_provider(cfg), cfg)
        {
            return CompactString::new(model);
        }
        if let Some(m) = cfg.model.as_deref() {
            return CompactString::new(m);
        }
        // No explicit model. If a provider was chosen explicitly, default to a
        // model valid for it so `--provider anthropic` does not keep the
        // OpenRouter default id; otherwise keep the historic deepseek default.
        if (self.provider.is_some() || cfg.provider.is_some())
            && let Some((model, _)) =
                crate::provider::default_model_for_provider(&self.resolve_provider(cfg), cfg)
        {
            return CompactString::new(model);
        }
        let qm = config::quick_models_map(cfg);
        qm.get("deepseek-v4-pro")
            .map(|q| q.model.clone())
            .unwrap_or_else(|| CompactString::new("deepseek/deepseek-v4-pro"))
    }

    pub fn resolve_provider(&self, cfg: &config::Config) -> CompactString {
        self.provider
            .as_deref()
            .or(cfg.provider.as_deref())
            .map(CompactString::new)
            .unwrap_or_else(|| {
                let qm = config::quick_models_map(cfg);
                qm.get("deepseek-v4-pro")
                    .map(|q| q.provider.clone())
                    .unwrap_or_else(|| CompactString::new("openrouter"))
            })
    }

    pub fn resolve_max_tokens(&self, cfg: &config::Config) -> u64 {
        self.max_tokens.or(cfg.max_tokens).unwrap_or(16384)
    }

    pub fn resolve_max_agent_turns(&self, cfg: &config::Config) -> usize {
        self.max_agent_turns.or(cfg.max_agent_turns).unwrap_or(200)
    }

    pub fn resolve_no_context_files(&self, cfg: &config::Config) -> bool {
        self.no_context_files || cfg.no_context_files.unwrap_or(false)
    }

    pub fn resolve_no_tools(&self, cfg: &config::Config) -> bool {
        self.no_tools || cfg.no_tools.unwrap_or(false)
    }

    pub fn resolve_sandbox(&self, cfg: &config::Config) -> bool {
        self.sandbox || cfg.sandbox.unwrap_or(false)
    }

    pub fn resolve_sandbox_backend(&self, cfg: &config::Config) -> String {
        self.sandbox_backend
            .clone()
            .or_else(|| cfg.sandbox_backend.clone())
            .unwrap_or_else(|| "bwrap".to_string())
    }

    pub fn resolve_shell(&self, cfg: &config::Config) -> String {
        self.shell
            .clone()
            .or_else(|| cfg.shell.clone())
            .unwrap_or_else(|| "bash".to_string())
    }

    pub fn resolve_edit_system(&self, cfg: &config::Config) -> EditSystem {
        self.edit_system
            .as_deref()
            .and_then(|s| s.parse().ok())
            .or(cfg.edit_system)
            .unwrap_or_default()
    }

    #[cfg(feature = "git-worktree")]
    pub fn resolve_wt_auto_merge(&self, cfg: &config::Config) -> bool {
        self.wt_auto_merge || self.parallel || cfg.wt_auto_merge.unwrap_or(false)
    }

    #[cfg(feature = "git-worktree")]
    pub fn resolve_wt_base_dir(&self, cfg: &config::Config) -> Option<std::path::PathBuf> {
        self.wt_base_dir
            .clone()
            .or_else(|| cfg.wt_base_dir.clone())
            .map(std::path::PathBuf::from)
    }

    #[cfg(feature = "git-worktree")]
    pub fn resolve_wt_force(&self, cfg: &config::Config) -> bool {
        self.wt_force || cfg.wt_force.unwrap_or(false)
    }

    #[cfg(feature = "advisor")]
    pub fn resolve_advisor_enabled(&self, cfg: &config::Config) -> bool {
        if let Some(ref ac) = cfg.advisor {
            self.advisor || ac.enabled
        } else {
            self.advisor
        }
    }

    #[cfg(feature = "advisor")]
    pub fn resolve_advisor_model(&self, cfg: &config::Config) -> String {
        self.advisor_model
            .clone()
            .or_else(|| {
                cfg.advisor
                    .as_ref()
                    .and_then(|a| a.model.clone())
                    .map(|m| m.to_string())
            })
            .unwrap_or_else(|| "deepseek-v4-pro".to_string())
    }

    #[cfg(feature = "advisor")]
    pub fn resolve_advisor_max_uses(&self, cfg: &config::Config) -> Option<usize> {
        self.advisor_max_uses
            .or_else(|| cfg.advisor.as_ref().and_then(|a| a.max_uses))
    }

    #[cfg(feature = "advisor")]
    pub fn resolve_advisor_human_handoff(&self, cfg: &config::Config) -> bool {
        self.advisor_human_handoff.unwrap_or_else(|| {
            cfg.advisor
                .as_ref()
                .map(|a| a.human_handoff)
                .unwrap_or(false)
        })
    }

    #[cfg(feature = "advisor")]
    pub fn resolve_advisor_kilobytes_limit(&self, cfg: &config::Config) -> u32 {
        if self.advisor_kilobytes_limit != 256 {
            self.advisor_kilobytes_limit
        } else {
            cfg.advisor
                .as_ref()
                .map(|a| a.advisor_kilobytes_limit)
                .unwrap_or(256)
        }
    }
}
