#[cfg(not(unix))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the native Emacs demo requires Unix sockets")
}

#[cfg(unix)]
mod unix_demo {
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use anyhow::{Context, anyhow};
    use base64::Engine;
    use base64::prelude::BASE64_STANDARD;
    use chrono::{Duration as ChronoDuration, Utc};
    use serde::Serialize;
    use serde_json::{Value, json};
    use uuid::Uuid;

    const PROVIDER: &str = "demo-openai";
    const MODEL: &str = "zerostack-demo-random";
    const API_KEY: &str = "dummy-demo-key";
    const EMACS_LISP: &str = include_str!("../emacs/zerostack.el");
    const DEMO_TOOL_SEQUENCE: &[&str] = &[
        "read",
        "list_dir",
        "find_files",
        "grep",
        "task",
        "write",
        "edit",
        "bash",
        "bash",
        "write_todo_list",
    ];
    const DEMO_RTK_BASH_COMMAND: &str = concat!(
        "printf 'RTK demo command: disable_rtk is omitted, so an rtk-enabled build wraps this call.\\n'; ",
        "printf 'rtk sample line %03d\\n' 1; ",
        "printf 'rtk sample line %03d\\n' 2; ",
        "pwd",
    );
    const DEMO_LONG_BASH_COMMAND: &str = concat!(
        "printf 'Raw live-output demo: disable_rtk=true bypasses RTK and writes a tailing artifact.\\n'; ",
        "i=0; ",
        "while [ \"$i\" -lt 260 ]; do ",
        "printf 'demo live output line %03d: this long bash output is saved outside session context for read-tool inspection\\n' \"$i\"; ",
        "i=$((i + 1)); ",
        "sleep 0.12; ",
        "done; pwd",
    );

    pub fn run() -> anyhow::Result<()> {
        let emacs = std::env::var_os("EMACS").unwrap_or_else(|| OsString::from("emacs"));
        let root = DemoRoot::new()?;
        let env = DemoEnv {
            root: root.path().to_path_buf(),
            data: root.path().join("d"),
            runtime: root.path().join("r"),
            config: root.path().join("c"),
            lisp: root.path().join("l"),
            projects: root.path().join("p"),
            logs: root.path().join("log"),
            attachments: root.path().join("att"),
        };
        env.create_dirs()?;
        fs::write(env.lisp.join("zerostack.el"), EMACS_LISP)?;
        install_demo_rtk_shim(&env)?;
        let provider = ProviderServer::start(env.attachments.clone())?;
        let provider_url = provider.base_url();
        write_config(&env.config, &provider_url)?;
        let zerostack = prepare_zerostack_binary(&env)?;

        let sessions = create_demo_projects_and_sessions(&env)?;
        let live_sessions: Vec<_> = sessions.iter().filter(|s| s.live).collect();

        println!("zerostack Emacs demo environment: {}", env.root.display());
        println!("local OpenAI-compatible provider: {provider_url}");
        println!("provider attachment dump: {}", env.attachments.display());
        println!("regular zerostack binary: {}", zerostack.display());
        println!("starting {} live session workers...", live_sessions.len());

        let mut workers = Vec::new();
        for (idx, session) in live_sessions.into_iter().enumerate() {
            let permissions = if idx == 0 {
                WorkerPermissions::Ask
            } else {
                WorkerPermissions::AcceptAll
            };
            let mut worker = start_worker(&zerostack, &env, session, permissions)?;
            wait_for_worker_socket(&env.runtime, &mut worker)
                .with_context(|| format!("waiting for session {} socket", worker.session_id))?;
            workers.push(worker);
        }
        let first_socket = workers
            .first()
            .map(|worker| worker_socket_path(&env.runtime, &worker.session_id));

        println!("launching Emacs board; close Emacs to remove the demo environment");
        let status = Command::new(&emacs)
            .arg("-Q")
            .arg("-L")
            .arg(&env.lisp)
            .arg("-l")
            .arg("zerostack")
            .arg("--eval")
            .arg(emacs_eval(&zerostack, first_socket.as_deref()))
            .current_dir(&env.root)
            .env("ZS_DATA_DIR", &env.data)
            .env("ZS_RUNTIME_DIR", &env.runtime)
            .env("ZS_CONFIG_DIR", &env.config)
            .env("PATH", demo_path(&env))
            .env("ZEROSTACK_DEMO_API_KEY", API_KEY)
            .status()
            .with_context(|| format!("launching {}", Path::new(&emacs).display()))?;

        for worker in &mut workers {
            worker.kill();
        }

        if !status.success() {
            anyhow::bail!("Emacs exited with status {status}");
        }
        Ok(())
    }

    fn emacs_eval(zerostack: &Path, first_socket: Option<&Path>) -> String {
        let command = emacs_lisp_string(&zerostack.to_string_lossy());
        let connect = first_socket
            .map(|socket| {
                let socket = emacs_lisp_string(&socket.to_string_lossy());
                let prompt = emacs_lisp_string(
                    "Show the native Emacs demo. Take multiple tool turns with visible thinking, use the project-local demo skills if relevant, exercise every available zerostack tool including a task subagent call, produce one long bash output that is saved outside the transcript, read the saved output path back with the read tool, then return markdown with a table, code block, task list, inline LaTeX, and display LaTeX. The first tool call should ask me for permission; I can answer with the inline permission buttons below the prompt. I may press C-c C-c to abort while you are thinking.",
                );
                format!(
                    "(let ((buf (zerostack-connect {socket}))) \
                      (run-at-time 1 nil \
                        (lambda () \
                          (when (buffer-live-p buf) \
                            (with-current-buffer buf \
                              (zerostack-send-prompt {prompt}))))))"
                )
            })
            .unwrap_or_default();
        format!(
            "(progn \
                (setq zerostack-command {command}) \
                (setq zerostack-auctex-preview t) \
                (setq zerostack-auctex-display-buffer nil) \
                (zerostack-board) \
                {connect})"
        )
    }

    fn emacs_lisp_string(value: &str) -> String {
        let mut out = String::from("\"");
        for ch in value.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    fn prepare_zerostack_binary(env: &DemoEnv) -> anyhow::Result<PathBuf> {
        if let Some(path) = std::env::var_os("ZEROSTACK_BIN") {
            return Ok(PathBuf::from(path));
        }

        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_zerostack") {
            return Ok(PathBuf::from(path));
        }

        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let manifest_path = manifest_dir.join("Cargo.toml");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
        println!(
            "compiling regular zerostack binary from {}...",
            manifest_path.display()
        );
        let output = Command::new(&cargo)
            .arg("run")
            .arg("--quiet")
            .arg("--manifest-path")
            .arg(&manifest_path)
            .arg("--bin")
            .arg("zerostack")
            .arg("--features")
            .arg("multimodal,rtk")
            .arg("--")
            .arg("--print-config")
            .current_dir(&manifest_dir)
            .env("ZS_DATA_DIR", &env.data)
            .env("ZS_RUNTIME_DIR", &env.runtime)
            .env("ZS_CONFIG_DIR", &env.config)
            .env("PATH", demo_path(env))
            .env("ZEROSTACK_DEMO_API_KEY", API_KEY)
            .output()
            .with_context(|| format!("running {}", Path::new(&cargo).display()))?;
        if !output.status.success() {
            return Err(anyhow!(
                "failed to compile/run regular zerostack binary with Cargo\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let binary = cargo_target_debug_dir(&manifest_dir).join("zerostack");
        if !binary.is_file() {
            anyhow::bail!(
                "Cargo succeeded, but expected regular zerostack binary at {}",
                binary.display()
            );
        }
        Ok(binary)
    }

    fn install_demo_rtk_shim(env: &DemoEnv) -> anyhow::Result<()> {
        let bin = env.root.join("bin");
        fs::create_dir_all(&bin)?;
        let rtk = bin.join("rtk");
        fs::write(
            &rtk,
            concat!(
                "#!/usr/bin/env bash\n",
                "printf '[demo rtk wrapper] executing via RTK path:'\n",
                "for arg in \"$@\"; do printf ' %q' \"$arg\"; done\n",
                "printf '\\n'\n",
                "exec \"$@\"\n",
            ),
        )?;
        let mut permissions = fs::metadata(&rtk)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&rtk, permissions)?;
        Ok(())
    }

    fn demo_path(env: &DemoEnv) -> OsString {
        let mut path = env.root.join("bin").into_os_string();
        if let Some(existing) = std::env::var_os("PATH") {
            path.push(":");
            path.push(existing);
        }
        path
    }

    fn cargo_target_debug_dir(manifest_dir: &Path) -> PathBuf {
        if let Some(target) = std::env::var_os("CARGO_TARGET_DIR") {
            let target = PathBuf::from(target);
            if target.is_absolute() {
                return target.join("debug");
            }
            return manifest_dir.join(target).join("debug");
        }
        manifest_dir.join("target").join("debug")
    }

    struct DemoRoot {
        path: PathBuf,
    }

    impl DemoRoot {
        fn new() -> anyhow::Result<Self> {
            let tmp = std::env::var_os("ZEROSTACK_DEMO_TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            let suffix = Uuid::new_v4().simple().to_string();
            let path = tmp.join(format!("zsd-{}", &suffix[..8]));
            fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for DemoRoot {
        fn drop(&mut self) {
            if std::env::var_os("ZEROSTACK_DEMO_KEEP").is_some() {
                eprintln!("keeping demo environment: {}", self.path.display());
            } else {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }

    struct DemoEnv {
        root: PathBuf,
        data: PathBuf,
        runtime: PathBuf,
        config: PathBuf,
        lisp: PathBuf,
        projects: PathBuf,
        logs: PathBuf,
        attachments: PathBuf,
    }

    impl DemoEnv {
        fn create_dirs(&self) -> anyhow::Result<()> {
            for dir in [
                &self.data,
                &self.runtime,
                &self.config,
                &self.lisp,
                &self.projects,
                &self.logs,
                &self.attachments,
                &self.data.join("sessions"),
            ] {
                fs::create_dir_all(dir)?;
            }
            Ok(())
        }
    }

    fn write_config(config_dir: &Path, base_url: &str) -> anyhow::Result<()> {
        let config = format!(
            r#"provider = "{PROVIDER}"
model = "{MODEL}"
no_context_files = true
max_agent_turns = 12
max_read_lines = 120
max_list_dir_entries = 80

[custom_providers.{PROVIDER}]
provider_type = "openai"
base_url = "{base_url}"
api_key_env = "ZEROSTACK_DEMO_API_KEY"
api_style = "completions"
model = "{MODEL}"
timeout_secs = 60
"#
        );
        fs::write(config_dir.join("config.toml"), config)?;
        Ok(())
    }

    #[derive(Clone)]
    struct DemoSessionSpec {
        id: String,
        name: String,
        cwd: PathBuf,
        live: bool,
    }

    fn create_demo_projects_and_sessions(env: &DemoEnv) -> anyhow::Result<Vec<DemoSessionSpec>> {
        let alpha = create_project(
            &env.projects,
            "alpha-engine",
            "Async runtime playground for rendered Markdown, tools, and math.",
        )?;
        let alpha_tables = add_worktree(
            &alpha.main,
            &env.projects,
            "alpha-engine-tables",
            "feature/rich-tables",
            "Render dense markdown tables and artifact links in Emacs.",
        )?;
        let alpha_latex = add_worktree(
            &alpha.main,
            &env.projects,
            "alpha-engine-latex",
            "feature/latex-preview",
            "Preview model math with AUCTeX after a turn is done.",
        )?;

        let beta = create_project(
            &env.projects,
            "beta-cli",
            "Small command-line app with branch descriptions and old sessions.",
        )?;
        let beta_cleanup = add_worktree(
            &beta.main,
            &env.projects,
            "beta-cli-cleanup",
            "refactor/session-board",
            "Reorganize project/worktree/session board views.",
        )?;
        let loose_workspace = create_loose_workspace(
            &env.projects,
            "scratch-nongit",
            "Non-Git workspace for board grouping demos.",
        )?;
        let loose_archive = create_loose_workspace(
            &env.projects,
            "archive-nongit",
            "Older non-Git workspace with dormant sessions.",
        )?;

        let mut specs = vec![
            seed_session(
                env,
                "Live table rendering",
                &alpha_tables,
                true,
                true,
                0,
                "Can you inspect README.md and show off tables, code, tool output, and LaTeX?",
            )?,
            seed_session(
                env,
                "Live math preview",
                &alpha_latex,
                true,
                false,
                1,
                "Please demonstrate reasoning artifacts and AUCTeX-ready math metadata.",
            )?,
            seed_session(
                env,
                "Dormant main branch notes",
                &alpha.main,
                false,
                false,
                2,
                "Summarize the project layout and planned worktrees.",
            )?,
            seed_session(
                env,
                "Inactive board refactor",
                &beta_cleanup,
                false,
                false,
                3,
                "Track the board sorting rules and inactive session behavior.",
            )?,
            seed_session(
                env,
                "Old beta main session",
                &beta.main,
                false,
                false,
                4,
                "Keep this older main-worktree session for ordering contrast.",
            )?,
        ];
        specs.push(seed_session_with_messages(
            env,
            "Huge transcript stress test",
            &alpha.main,
            false,
            false,
            5,
            huge_demo_messages(),
        )?);
        for idx in 0..12 {
            specs.push(seed_session(
                env,
                &format!("Saved crowded workspace session {:02}", idx + 1,),
                &alpha_tables,
                false,
                false,
                idx as i64,
                &format!(
                    "Session {} in a crowded worktree used to demonstrate board pagination.",
                    idx + 1
                ),
            )?);
        }
        specs.push(seed_session(
            env,
            "Loose scratch notes",
            &loose_workspace,
            false,
            false,
            1,
            "This session belongs to a directory with no Git repository.",
        )?);
        specs.push(seed_session(
            env,
            "Loose pasted artifact review",
            &loose_workspace,
            false,
            false,
            2,
            "Check how non-Git sessions are grouped separately on the board.",
        )?);
        specs.push(seed_session(
            env,
            "Archived non-Git session",
            &loose_archive,
            false,
            false,
            8,
            "Old non-Git workspace session for category ordering contrast.",
        )?);

        Ok(specs)
    }

    struct ProjectPaths {
        main: PathBuf,
    }

    fn create_project(base: &Path, name: &str, description: &str) -> anyhow::Result<ProjectPaths> {
        let repo = base.join(name);
        fs::create_dir_all(repo.join("src"))?;
        run_git_maybe(&repo, ["init", "-b", "main"]).or_else(|_| run_git(&repo, ["init"]))?;
        run_git(&repo, ["checkout", "-B", "main"])?;
        run_git(&repo, ["config", "user.name", "Zerostack Demo"])?;
        run_git(&repo, ["config", "user.email", "demo@example.invalid"])?;
        run_git(&repo, ["config", "branch.main.description", description])?;

        fs::write(
            repo.join("README.md"),
            format!(
                "# {name}\n\n{description}\n\n- [x] boot isolated Emacs demo\n- [ ] ask zerostack to inspect this file\n\n| Area | Status |\n| --- | --- |\n| markdown | ready |\n| tools | ready |\n| math | $E = mc^2$ |\n"
            ),
        )?;
        fs::write(
            repo.join("src/main.rs"),
            "fn main() {\n    println!(\"hello from the zerostack demo\");\n}\n",
        )?;
        write_demo_skills(&repo)?;
        run_git(&repo, ["add", "."])?;
        run_git(&repo, ["commit", "-m", "Initial demo project"])?;

        Ok(ProjectPaths { main: repo })
    }

    fn add_worktree(
        repo: &Path,
        base: &Path,
        dirname: &str,
        branch: &str,
        description: &str,
    ) -> anyhow::Result<PathBuf> {
        let path = base.join(dirname);
        run_git(repo, ["worktree", "add", "-b", branch, path_str(&path)?])?;
        run_git(
            repo,
            [
                "config",
                &format!("branch.{branch}.description"),
                description,
            ],
        )?;
        fs::write(
            path.join("WORKTREE_NOTES.md"),
            format!(
                "# {branch}\n\n{description}\n\nThis file exists so the demo provider can call the real `read` tool.\n"
            ),
        )?;
        run_git(&path, ["add", "."])?;
        run_git(&path, ["commit", "-m", "Add worktree notes"])?;
        Ok(path)
    }

    fn create_loose_workspace(
        base: &Path,
        dirname: &str,
        description: &str,
    ) -> anyhow::Result<PathBuf> {
        let path = base.join(dirname);
        fs::create_dir_all(&path)?;
        fs::write(
            path.join("NOTES.md"),
            format!(
                "# {dirname}\n\n{description}\n\nThis directory is intentionally not a Git repository.\n"
            ),
        )?;
        Ok(path)
    }

    fn write_demo_skills(repo: &Path) -> anyhow::Result<()> {
        write_demo_skill(
            &repo.join(".claude").join("skills").join("render-review"),
            "render-review",
            "Review rendered Markdown, artifacts, and LaTeX output in the native Emacs demo.",
            "Use this skill when the user asks about rendered Markdown, inline LaTeX SVGs, table layout, or artifact links in the native Emacs demo. Check the final answer for a table, task list, code block, inline math, display math, and clickable artifacts.",
        )?;
        write_demo_skill(
            &repo.join(".opencode").join("skills").join("tool-tour"),
            "tool-tour",
            "Walk through every built-in tool that the native Emacs demo provider offers.",
            "Use this skill when the user asks for a demo tour. Mention that the demo intentionally exercises read, list_dir, find_files, grep, task subagents, write, edit, bash, and write_todo_list, then reads back the saved sidecar path for a deliberately long bash output. Delayed reasoning lets C-c C-c interruption be tested.",
        )?;
        Ok(())
    }

    fn write_demo_skill(
        dir: &Path,
        name: &str,
        description: &str,
        body: &str,
    ) -> anyhow::Result<()> {
        fs::create_dir_all(dir)?;
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
        )?;
        Ok(())
    }

    fn run_git_maybe<const N: usize>(cwd: &Path, args: [&str; N]) -> anyhow::Result<()> {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("git command failed"))
        }
    }

    fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> anyhow::Result<()> {
        let output = Command::new("git").args(args).current_dir(cwd).output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "git failed in {}: {}",
                cwd.display(),
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }

    fn path_str(path: &Path) -> anyhow::Result<&str> {
        path.to_str()
            .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
    }

    fn seed_session(
        env: &DemoEnv,
        name: &str,
        cwd: &Path,
        live: bool,
        allow_demo_tools_after_first_permission: bool,
        age_days: i64,
        prompt: &str,
    ) -> anyhow::Result<DemoSessionSpec> {
        let assistant = format!(
            "Seeded demo session for `{}`. Ask a prompt to see streaming markdown, tool artifacts, and LaTeX metadata.",
            cwd.file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("worktree")
        );
        let messages = vec![
            DemoMessage::new("user", prompt),
            DemoMessage::new("assistant", &assistant),
        ];
        seed_session_with_messages(
            env,
            name,
            cwd,
            live,
            allow_demo_tools_after_first_permission,
            age_days,
            messages,
        )
    }

    fn seed_session_with_messages(
        env: &DemoEnv,
        name: &str,
        cwd: &Path,
        live: bool,
        allow_demo_tools_after_first_permission: bool,
        age_days: i64,
        messages: Vec<DemoMessage>,
    ) -> anyhow::Result<DemoSessionSpec> {
        let id = Uuid::new_v4().to_string();
        let updated_at = Utc::now() - ChronoDuration::days(age_days);
        let created_at = updated_at - ChronoDuration::minutes(17);
        let total_estimated_tokens = messages.iter().map(|m| m.estimated_tokens).sum();
        let session = DemoSession {
            id: id.clone(),
            name: name.to_string(),
            messages,
            compactions: Vec::<Value>::new(),
            created_at: created_at.to_rfc3339(),
            updated_at: updated_at.to_rfc3339(),
            total_input_tokens: 120,
            total_output_tokens: 45,
            total_cost: 0.0,
            total_estimated_tokens,
            calibrated_tokens: 0,
            calibrated_msg_count: 0,
            input_token_cost: 0.0,
            output_token_cost: 0.0,
            context_window: 128_000,
            model: MODEL.to_string(),
            provider: PROVIDER.to_string(),
            working_dir: cwd.to_string_lossy().to_string(),
            permission_allowlist: if allow_demo_tools_after_first_permission {
                demo_permission_allowlist_after_first_tool()
            } else {
                Vec::new()
            },
        };
        let json = serde_json::to_string_pretty(&session)?;
        fs::write(env.data.join("sessions").join(format!("{id}.json")), json)?;
        Ok(DemoSessionSpec {
            id,
            name: name.to_string(),
            cwd: cwd.to_path_buf(),
            live,
        })
    }

    fn huge_demo_messages() -> Vec<DemoMessage> {
        let mut messages = Vec::new();
        messages.push(DemoMessage::new(
            "user",
            "Create a long-running architecture review with enough text to stress the Emacs transcript renderer.",
        ));
        for turn in 1..=80 {
            messages.push(DemoMessage::new(
                "assistant",
                &format!(
                    "## Huge transcript section {turn}\n\n{}\n\n| Area | Detail |\n| --- | --- |\n| renderer | preserves point while replacing rendered markdown |\n| board | keeps huge sessions discoverable without flooding rows |\n| math | $E = mc^2$ and $\\alpha + \\beta$ remain inline |\n\n```rust\nfn section_{turn}() {{ println!(\"large demo transcript\"); }}\n```\n",
                    "This seeded assistant message repeats enough explanatory prose to make the session visibly large when opened. ".repeat(18)
                ),
            ));
            messages.push(DemoMessage::new(
                "user",
                &format!(
                    "Continue section {turn} with more notes about the native Emacs board demo."
                ),
            ));
        }
        messages
    }

    #[derive(Serialize)]
    struct DemoSession {
        id: String,
        name: String,
        messages: Vec<DemoMessage>,
        compactions: Vec<Value>,
        created_at: String,
        updated_at: String,
        total_input_tokens: u64,
        total_output_tokens: u64,
        total_cost: f64,
        total_estimated_tokens: u64,
        calibrated_tokens: u64,
        calibrated_msg_count: usize,
        input_token_cost: f64,
        output_token_cost: f64,
        context_window: u64,
        model: String,
        provider: String,
        working_dir: String,
        permission_allowlist: Vec<DemoPermissionAllowEntry>,
    }

    #[derive(Serialize, Clone)]
    struct DemoPermissionAllowEntry {
        tool: &'static str,
        pattern: &'static str,
    }

    fn demo_permission_allowlist_after_first_tool() -> Vec<DemoPermissionAllowEntry> {
        vec![
            DemoPermissionAllowEntry {
                tool: "list_dir",
                pattern: "**",
            },
            DemoPermissionAllowEntry {
                tool: "find_files",
                pattern: "**",
            },
            DemoPermissionAllowEntry {
                tool: "grep",
                pattern: "**",
            },
            DemoPermissionAllowEntry {
                tool: "task",
                pattern: "**",
            },
            DemoPermissionAllowEntry {
                tool: "write",
                pattern: "demo-output/**",
            },
            DemoPermissionAllowEntry {
                tool: "edit",
                pattern: "demo-output/**",
            },
            DemoPermissionAllowEntry {
                tool: "bash",
                pattern: "**",
            },
            DemoPermissionAllowEntry {
                tool: "write_todo_list",
                pattern: "**",
            },
        ]
    }

    #[derive(Serialize)]
    struct DemoMessage {
        role: &'static str,
        content: String,
        estimated_tokens: u64,
    }

    impl DemoMessage {
        fn new(role: &'static str, content: &str) -> Self {
            Self {
                role,
                content: content.to_string(),
                estimated_tokens: estimate_tokens(content),
            }
        }
    }

    fn estimate_tokens(text: &str) -> u64 {
        (text.chars().count() as u64 / 4).max(1)
    }

    struct Worker {
        session_id: String,
        child: Child,
        log_path: PathBuf,
    }

    impl Worker {
        fn kill(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            self.kill();
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum WorkerPermissions {
        Ask,
        AcceptAll,
    }

    fn start_worker(
        zerostack: &Path,
        env: &DemoEnv,
        session: &DemoSessionSpec,
        permissions: WorkerPermissions,
    ) -> anyhow::Result<Worker> {
        let log_path = env.logs.join(format!("{}.stderr", &session.id[..8]));
        let stderr = File::create(&log_path)?;
        let mut command = Command::new(zerostack);
        command
            .arg("--emacs")
            .arg("--session")
            .arg(&session.id)
            .arg("--provider")
            .arg(PROVIDER)
            .arg("--model")
            .arg(MODEL)
            .args(worker_permission_args(permissions))
            .arg("--no-context-files")
            .current_dir(&session.cwd)
            .env("ZS_DATA_DIR", &env.data)
            .env("ZS_RUNTIME_DIR", &env.runtime)
            .env("ZS_CONFIG_DIR", &env.config)
            .env("PATH", demo_path(env))
            .env("ZEROSTACK_DEMO_API_KEY", API_KEY)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        let child = command
            .spawn()
            .with_context(|| format!("starting zerostack worker for {}", session.name))?;
        Ok(Worker {
            session_id: session.id.clone(),
            child,
            log_path,
        })
    }

    fn worker_permission_args(permissions: WorkerPermissions) -> [&'static str; 1] {
        match permissions {
            WorkerPermissions::Ask => ["--restrictive"],
            WorkerPermissions::AcceptAll => ["--accept-all"],
        }
    }

    fn wait_for_worker_socket(runtime: &Path, worker: &mut Worker) -> anyhow::Result<()> {
        let socket = worker_socket_path(runtime, &worker.session_id);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10) {
            if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                return Ok(());
            }
            if let Some(status) = worker.child.try_wait()? {
                return Err(anyhow!(
                    "worker exited with {status}; stderr:\n{}",
                    read_log_tail(&worker.log_path)
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(anyhow!(
            "socket did not become live: {}; stderr:\n{}",
            socket.display(),
            read_log_tail(&worker.log_path)
        ))
    }

    fn worker_socket_path(runtime: &Path, session_id: &str) -> PathBuf {
        runtime.join("sessions").join(session_id).join("sock")
    }

    fn read_log_tail(path: &Path) -> String {
        fs::read_to_string(path)
            .map(|text| {
                let lines: Vec<&str> = text.lines().rev().take(40).collect();
                lines.into_iter().rev().collect::<Vec<_>>().join("\n")
            })
            .unwrap_or_else(|_| format!("<could not read {}>", path.display()))
    }

    struct ProviderServer {
        addr: SocketAddr,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
        cleanup_attachment_dir: Option<PathBuf>,
    }

    impl ProviderServer {
        fn start(attachment_dir: PathBuf) -> anyhow::Result<Self> {
            Self::start_with_attachment_dir(
                demo_delay(),
                attachment_dir,
                false,
                demo_transient_failures(),
            )
        }

        #[cfg(test)]
        fn start_with_delay(delay: Duration) -> anyhow::Result<Self> {
            Self::start_with_transient_failures(delay, 0)
        }

        #[cfg(test)]
        fn start_with_transient_failures(
            delay: Duration,
            transient_failures: u64,
        ) -> anyhow::Result<Self> {
            let path =
                std::env::temp_dir().join(format!("zsd-provider-att-{}", Uuid::new_v4().simple()));
            Self::start_with_attachment_dir(delay, path, true, transient_failures)
        }

        fn start_with_attachment_dir(
            delay: Duration,
            attachment_dir: PathBuf,
            cleanup_attachment_dir: bool,
            transient_failures: u64,
        ) -> anyhow::Result<Self> {
            fs::create_dir_all(&attachment_dir)?;
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            listener.set_nonblocking(true)?;
            let addr = listener.local_addr()?;
            let stop = Arc::new(AtomicBool::new(false));
            let state = Arc::new(ProviderState {
                counter: AtomicU64::new(0),
                transient_failures: AtomicU64::new(transient_failures),
                attachment_dir,
            });
            let cleanup_path = cleanup_attachment_dir.then(|| state.attachment_dir.clone());
            let thread_stop = Arc::clone(&stop);
            let handle = thread::spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let state = Arc::clone(&state);
                            thread::spawn(move || {
                                let _ = handle_http_connection(stream, state, delay);
                            });
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => break,
                    }
                }
            });
            Ok(Self {
                addr,
                stop,
                handle: Some(handle),
                cleanup_attachment_dir: cleanup_path,
            })
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }
    }

    impl Drop for ProviderServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.addr);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
            if let Some(path) = &self.cleanup_attachment_dir {
                let _ = fs::remove_dir_all(path);
            }
        }
    }

    struct ProviderState {
        counter: AtomicU64,
        transient_failures: AtomicU64,
        attachment_dir: PathBuf,
    }

    struct HttpRequest {
        method: String,
        path: String,
        body: Vec<u8>,
    }

    fn handle_http_connection(
        mut stream: TcpStream,
        state: Arc<ProviderState>,
        delay: Duration,
    ) -> anyhow::Result<()> {
        let request = read_http_request(&mut stream)?;
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/models") | ("GET", "/v1/models") => {
                write_json_response(&mut stream, 200, models_response())?;
            }
            ("POST", "/chat/completions") | ("POST", "/v1/chat/completions") => {
                if state
                    .transient_failures
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_ok()
                {
                    write_json_response(
                        &mut stream,
                        503,
                        json!({ "error": { "message": "demo provider overloaded; retry should recover" } }),
                    )?;
                    return Ok(());
                }
                let body: Value =
                    serde_json::from_slice(&request.body).unwrap_or_else(|_| json!({}));
                let sequence = state.counter.fetch_add(1, Ordering::Relaxed) + 1;
                let frames = chat_completion_frames_with_attachments(
                    &body,
                    sequence,
                    &state.attachment_dir,
                )?;
                write_sse_response(&mut stream, &frames, delay)?;
            }
            _ => {
                write_json_response(
                    &mut stream,
                    404,
                    json!({ "error": { "message": "unknown demo endpoint" } }),
                )?;
            }
        }
        Ok(())
    }

    fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<HttpRequest> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut buffer = Vec::new();
        let mut scratch = [0u8; 1024];
        let header_end = loop {
            let n = stream.read(&mut scratch)?;
            if n == 0 {
                return Err(anyhow!("connection closed before headers"));
            }
            buffer.extend_from_slice(&scratch[..n]);
            if let Some(pos) = find_header_end(&buffer) {
                break pos;
            }
            if buffer.len() > 128 * 1024 {
                return Err(anyhow!("request headers too large"));
            }
        };

        let header_text = String::from_utf8_lossy(&buffer[..header_end]);
        let mut lines = header_text.lines();
        let request_line = lines
            .next()
            .ok_or_else(|| anyhow!("missing request line"))?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let mut content_length = 0usize;
        for line in lines {
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }

        let body_start = header_end + 4;
        while buffer.len() < body_start + content_length {
            let n = stream.read(&mut scratch)?;
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&scratch[..n]);
        }
        let body = buffer[body_start..buffer.len().min(body_start + content_length)].to_vec();
        Ok(HttpRequest { method, path, body })
    }

    fn find_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn write_json_response(
        stream: &mut TcpStream,
        status: u16,
        value: Value,
    ) -> anyhow::Result<()> {
        let reason = match status {
            200 => "OK",
            404 => "Not Found",
            503 => "Service Unavailable",
            _ => "Error",
        };
        let body = serde_json::to_vec(&value)?;
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(&body)?;
        Ok(())
    }

    fn write_sse_response(
        stream: &mut TcpStream,
        frames: &[String],
        delay: Duration,
    ) -> anyhow::Result<()> {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
        )?;
        stream.flush()?;
        for frame in frames {
            stream.write_all(frame.as_bytes())?;
            stream.flush()?;
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }
        Ok(())
    }

    fn demo_transient_failures() -> u64 {
        std::env::var("ZEROSTACK_DEMO_TRANSIENT_FAILURES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(2)
    }

    fn demo_delay() -> Duration {
        let ms = std::env::var("ZEROSTACK_DEMO_DELAY_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(450);
        Duration::from_millis(ms)
    }

    fn models_response() -> Value {
        json!({
            "object": "list",
            "data": [{
                "id": MODEL,
                "object": "model",
                "created": 0,
                "owned_by": "zerostack-demo"
            }]
        })
    }

    #[cfg(test)]
    fn chat_completion_sse(request: &Value, sequence: u64) -> String {
        chat_completion_frames(request, sequence).concat()
    }

    fn chat_completion_frames_with_attachments(
        request: &Value,
        sequence: u64,
        attachment_dir: &Path,
    ) -> anyhow::Result<Vec<String>> {
        let attachments = save_request_attachments(request, sequence, attachment_dir)?;
        if attachments.is_empty() {
            Ok(chat_completion_frames(request, sequence))
        } else {
            Ok(attachment_response_frames(sequence, &attachments))
        }
    }

    fn chat_completion_frames(request: &Value, sequence: u64) -> Vec<String> {
        let tool = choose_tool(request);
        let reasoning_effort = request
            .get("reasoning")
            .and_then(|reasoning| reasoning.get("effort"))
            .and_then(Value::as_str)
            .filter(|effort| matches!(*effort, "minimal" | "low" | "medium" | "high"));

        let mut frames = Vec::new();
        for reasoning in demo_reasoning_chunks(sequence, tool, reasoning_effort) {
            frames.push(sse_chunk(json!({
                "id": format!("chatcmpl-demo-{sequence}"),
                "object": "chat.completion.chunk",
                "created": sequence,
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "delta": { "reasoning_content": reasoning },
                    "finish_reason": null
                }]
            })));
        }

        if let Some(tool) = tool {
            frames.push(sse_chunk(json!({
                "id": format!("chatcmpl-demo-{sequence}"),
                "object": "chat.completion.chunk",
                "created": sequence,
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": format!("call_demo_{sequence}"),
                            "type": "function",
                            "function": {
                                "name": tool,
                                "arguments": demo_tool_arguments(tool, request)
                            }
                        }]
                    },
                    "finish_reason": null
                }]
            })));
            frames.push(sse_chunk(json!({
                "id": format!("chatcmpl-demo-{sequence}"),
                "object": "chat.completion.chunk",
                "created": sequence,
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "tool_calls"
                }]
            })));
        } else {
            for part in split_response(&demo_markdown_response(sequence)) {
                frames.push(sse_chunk(json!({
                    "id": format!("chatcmpl-demo-{sequence}"),
                    "object": "chat.completion.chunk",
                    "created": sequence,
                    "model": MODEL,
                    "choices": [{
                        "index": 0,
                        "delta": { "content": part },
                        "finish_reason": null
                    }]
                })));
            }
            frames.push(sse_chunk(json!({
                "id": format!("chatcmpl-demo-{sequence}"),
                "object": "chat.completion.chunk",
                "created": sequence,
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 256 + sequence,
                    "completion_tokens": 96,
                    "total_tokens": 352 + sequence
                }
            })));
        }

        frames.push("data: [DONE]\n\n".to_string());
        frames
    }

    #[derive(Clone, Debug)]
    struct SavedAttachment {
        kind: &'static str,
        path: PathBuf,
        mime: String,
        bytes: usize,
    }

    fn save_request_attachments(
        request: &Value,
        sequence: u64,
        attachment_dir: &Path,
    ) -> anyhow::Result<Vec<SavedAttachment>> {
        fs::create_dir_all(attachment_dir)?;
        let mut saved = Vec::new();
        let Some(messages) = request.get("messages").and_then(Value::as_array) else {
            return Ok(saved);
        };

        for message in messages {
            let Some(parts) = message.get("content").and_then(Value::as_array) else {
                continue;
            };
            for part in parts {
                if let Some(attachment) =
                    save_content_part_attachment(part, sequence, saved.len() + 1, attachment_dir)?
                {
                    saved.push(attachment);
                }
            }
        }
        Ok(saved)
    }

    fn save_content_part_attachment(
        part: &Value,
        sequence: u64,
        index: usize,
        attachment_dir: &Path,
    ) -> anyhow::Result<Option<SavedAttachment>> {
        match part.get("type").and_then(Value::as_str) {
            Some("image_url") => {
                let Some(url) = part
                    .get("image_url")
                    .and_then(|image| image.get("url"))
                    .and_then(Value::as_str)
                else {
                    return Ok(None);
                };
                save_data_url_attachment(url, "image", sequence, index, attachment_dir)
            }
            Some("file") => {
                let Some(data) = part
                    .get("file")
                    .and_then(|file| file.get("file_data"))
                    .and_then(Value::as_str)
                else {
                    return Ok(None);
                };
                save_data_url_attachment(data, "file", sequence, index, attachment_dir)
            }
            Some("audio") => {
                let Some(input_audio) = part.get("input_audio") else {
                    return Ok(None);
                };
                let Some(data) = input_audio.get("data").and_then(Value::as_str) else {
                    return Ok(None);
                };
                let format = input_audio
                    .get("format")
                    .and_then(Value::as_str)
                    .unwrap_or("mp3");
                let mime = audio_format_mime(format);
                let bytes = BASE64_STANDARD
                    .decode(data)
                    .with_context(|| format!("decoding demo {mime} attachment"))?;
                Ok(Some(write_saved_attachment(
                    "audio",
                    mime,
                    bytes,
                    sequence,
                    index,
                    attachment_dir,
                )?))
            }
            _ => Ok(None),
        }
    }

    fn save_data_url_attachment(
        data_url: &str,
        kind: &'static str,
        sequence: u64,
        index: usize,
        attachment_dir: &Path,
    ) -> anyhow::Result<Option<SavedAttachment>> {
        let Some((mime, data)) = decode_data_url(data_url)? else {
            return Ok(None);
        };
        Ok(Some(write_saved_attachment(
            kind,
            &mime,
            data,
            sequence,
            index,
            attachment_dir,
        )?))
    }

    fn decode_data_url(value: &str) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        let Some(rest) = value.strip_prefix("data:") else {
            return Ok(None);
        };
        let Some((meta, data)) = rest.split_once(',') else {
            return Ok(None);
        };
        let mime = meta
            .split(';')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string();
        if !meta
            .split(';')
            .any(|part| part.eq_ignore_ascii_case("base64"))
        {
            return Ok(None);
        }
        let bytes = BASE64_STANDARD
            .decode(data)
            .with_context(|| format!("decoding demo {mime} data URL"))?;
        Ok(Some((mime, bytes)))
    }

    fn write_saved_attachment(
        kind: &'static str,
        mime: &str,
        data: Vec<u8>,
        sequence: u64,
        index: usize,
        attachment_dir: &Path,
    ) -> anyhow::Result<SavedAttachment> {
        let extension = mime_extension(mime);
        let path = attachment_dir.join(format!(
            "request-{sequence:04}-attachment-{index:02}.{extension}"
        ));
        fs::write(&path, &data)?;
        Ok(SavedAttachment {
            kind,
            path,
            mime: mime.to_string(),
            bytes: data.len(),
        })
    }

    fn attachment_response_frames(sequence: u64, attachments: &[SavedAttachment]) -> Vec<String> {
        let mut response = String::from("# Demo attachments received\n\n");
        response.push_str(
            "The demo provider detected attachments and skipped the normal multi-tool loop.\n\n",
        );
        response.push_str("Saved files:\n");
        for attachment in attachments {
            response.push_str(&format!(
                "- `{}` ({}, {}, {} bytes)\n",
                attachment.path.display(),
                attachment.kind,
                attachment.mime,
                attachment.bytes
            ));
        }

        let mut frames = Vec::new();
        for part in split_response(&response) {
            frames.push(sse_chunk(json!({
                "id": format!("chatcmpl-demo-{sequence}"),
                "object": "chat.completion.chunk",
                "created": sequence,
                "model": MODEL,
                "choices": [{
                    "index": 0,
                    "delta": { "content": part },
                    "finish_reason": null
                }]
            })));
        }
        frames.push(sse_chunk(json!({
            "id": format!("chatcmpl-demo-{sequence}"),
            "object": "chat.completion.chunk",
            "created": sequence,
            "model": MODEL,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 128 + sequence,
                "completion_tokens": 32,
                "total_tokens": 160 + sequence
            }
        })));
        frames.push("data: [DONE]\n\n".to_string());
        frames
    }

    fn audio_format_mime(format: &str) -> &'static str {
        match format {
            "wav" => "audio/wav",
            "ogg" => "audio/ogg",
            "flac" => "audio/flac",
            "m4a" => "audio/mp4",
            "aac" => "audio/aac",
            _ => "audio/mpeg",
        }
    }

    fn mime_extension(mime: &str) -> &'static str {
        match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "application/pdf" => "pdf",
            "audio/wav" => "wav",
            "audio/ogg" => "ogg",
            "audio/flac" => "flac",
            "audio/mp4" => "m4a",
            "audio/aac" => "aac",
            "audio/mpeg" => "mp3",
            _ => "bin",
        }
    }

    fn sse_chunk(value: Value) -> String {
        format!("data: {}\n\n", value)
    }

    fn request_tool_result_count(request: &Value) -> usize {
        request
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| {
                messages
                    .iter()
                    .filter(|message| {
                        message.get("role").and_then(Value::as_str) == Some("tool")
                            || message.get("tool_call_id").is_some()
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn choose_tool(request: &Value) -> Option<&'static str> {
        let names = tool_names(request);
        let used = used_tool_names(request);
        let result_count = request_tool_result_count(request);
        let mut advertised_index = 0usize;
        let mut preferred_seen = std::collections::HashMap::<&str, usize>::new();
        for preferred in DEMO_TOOL_SEQUENCE {
            if names.iter().any(|name| name == preferred) {
                let fallback_used = advertised_index < result_count;
                advertised_index += 1;
                let seen = preferred_seen.entry(*preferred).or_insert(0);
                *seen += 1;
                let used_count = used
                    .iter()
                    .filter(|name| name.as_str() == *preferred)
                    .count();
                if !fallback_used && used_count < *seen {
                    return Some(*preferred);
                }
            }
        }
        if names.iter().any(|name| name == "read")
            && saved_tool_output_path(request)
                .as_deref()
                .is_some_and(|path| !read_requested_for_path(request, path))
        {
            return Some("read");
        }
        None
    }

    fn used_tool_names(request: &Value) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(messages) = request.get("messages").and_then(Value::as_array) {
            for message in messages {
                if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for tool_call in tool_calls {
                        if let Some(name) = tool_call
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                        {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
        names
    }

    fn used_tool_count(request: &Value, tool: &str) -> usize {
        used_tool_names(request)
            .iter()
            .filter(|name| name.as_str() == tool)
            .count()
    }

    fn tool_names(request: &Value) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(tools) = request.get("tools").and_then(Value::as_array) {
            for tool in tools {
                if let Some(name) = tool
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                {
                    names.push(name.to_string());
                }
            }
        }
        if let Some(functions) = request.get("functions").and_then(Value::as_array) {
            for function in functions {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    names.push(name.to_string());
                }
            }
        }
        names
    }

    fn saved_tool_output_path(request: &Value) -> Option<String> {
        let messages = request.get("messages").and_then(Value::as_array)?;
        messages.iter().find_map(|message| {
            let content = message.get("content")?.as_str()?;
            extract_saved_tool_output_path(content)
        })
    }

    fn extract_saved_tool_output_path(content: &str) -> Option<String> {
        let marker = "[full output saved to: ";
        let start = content.find(marker)? + marker.len();
        let rest = &content[start..];
        let end = rest
            .find(';')
            .or_else(|| rest.find(']'))
            .unwrap_or(rest.len());
        let path = rest[..end].trim();
        (!path.is_empty()).then(|| path.to_string())
    }

    fn read_requested_for_path(request: &Value, path: &str) -> bool {
        let Some(messages) = request.get("messages").and_then(Value::as_array) else {
            return false;
        };
        messages.iter().any(|message| {
            if message
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| {
                    content.contains("[ToolCall]: read") && content.contains(path)
                })
            {
                return true;
            }
            let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
                return false;
            };
            tool_calls.iter().any(|tool_call| {
                let Some(function) = tool_call.get("function") else {
                    return false;
                };
                function.get("name").and_then(Value::as_str) == Some("read")
                    && function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .is_some_and(|args| args.contains(path))
            })
        })
    }

    fn demo_tool_arguments(tool: &str, request: &Value) -> String {
        let generated_path = demo_generated_path(request);
        match tool {
            "read" => {
                if let Some(path) = saved_tool_output_path(request)
                    .filter(|path| !read_requested_for_path(request, path))
                {
                    json!({ "path": path, "offset": 1, "limit": 80 }).to_string()
                } else {
                    json!({ "path": "README.md", "offset": 1, "limit": 80 }).to_string()
                }
            }
            "list_dir" => json!({ "path": "." }).to_string(),
            "find_files" => json!({ "pattern": "README|WORKTREE|SKILL|main\\.rs|generated", "path": "." }).to_string(),
            "grep" => json!({ "pattern": "demo|Zerostack|skill|LaTeX", "path": ".", "context_lines": 1 }).to_string(),
            "task" => json!({
                "prompts": [
                    "Inspect the demo workspace and summarize the project-local skills and fixture files that make this native Emacs demo interesting. Keep it concise."
                ]
            })
            .to_string(),
            "write" => json!({
                "path": generated_path,
                "content": "# Generated by the write tool\n\nstatus: draft\n"
            })
            .to_string(),
            "edit" => json!({
                "path": generated_path,
                "block": "<<<<<<< SEARCH\nstatus: draft\n=======\nstatus: edited by the edit tool\n>>>>>>> REPLACE\n"
            })
            .to_string(),
            "bash" => {
                if used_tool_count(request, "bash") == 0 {
                    json!({ "command": DEMO_RTK_BASH_COMMAND, "timeout": 1000 }).to_string()
                } else {
                    json!({ "command": DEMO_LONG_BASH_COMMAND, "timeout": 45000, "disable_rtk": true }).to_string()
                }
            }
            "write_todo_list" => json!({
                "todos": [
                    { "content": "Render board", "status": "completed", "priority": "high" },
                    { "content": "Stream thinking", "status": "in_progress", "priority": "high" },
                    { "content": "Try abort with C-c C-c", "status": "pending", "priority": "medium" }
                ]
            })
            .to_string(),
            _ => json!({}).to_string(),
        }
    }

    fn demo_generated_path(request: &Value) -> String {
        let user_messages = request
            .get("messages")
            .and_then(Value::as_array)
            .map(|messages| {
                messages
                    .iter()
                    .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
                    .count()
            })
            .unwrap_or(1)
            .max(1);
        format!("demo-output/generated-{user_messages}.md")
    }

    fn demo_reasoning_chunks(
        sequence: u64,
        next_tool: Option<&str>,
        reasoning_effort: Option<&str>,
    ) -> Vec<String> {
        let next = next_tool.unwrap_or("final response");
        let effort = reasoning_effort.unwrap_or("default");
        vec![
            format!(
                "Demo reasoning #{sequence}: reasoning effort is {effort}; inspect the conversation and decide the next action. "
            ),
            format!(
                "Next action: {next}. The demo provider acknowledged reasoning effort {effort} in this visible reasoning chunk."
            ),
        ]
    }

    fn demo_markdown_response(sequence: u64) -> String {
        let motif = match sequence % 3 {
            0 => "nebula",
            1 => "copper",
            _ => "aurora",
        };
        format!(
            r#"# Zerostack Emacs demo: {motif}

The local OpenAI-compatible provider returned **bold text**, *italic text*, `inline code`, and a [link](https://example.invalid) through the regular zerostack provider stack.

> Reasoning and tool output were written as ephemeral artifacts under the live session runtime directory.
> The long bash result is saved under the session tool-output directory and read back through the regular read tool.
> This quoted line should use the quote face.

- [x] styled span sexps
- [x] real tool calls and tool-output artifacts
- [x] built-in tool tour: read, list_dir, find_files, grep, task subagent, write, edit, bash, write_todo_list
- [x] saved long tool output path discovered from the transcript and inspected with read
- [x] project-local skills discovered from .claude/skills and .opencode/skills
- [x] Rust-rendered inline LaTeX SVG artifacts
- [ ] try `/compact` or open `M-x zerostack-board`

| Feature | What to look for | Score |
| :--- | :---: | ---: |
| Streaming | assistant lines replace in place | 9 |
| Tool output | clickable ephemeral artifact link | 10 |
| LaTeX | inline $E = mc^2$ and display metadata | 8 |
| Board | live sessions sorted first | 7 |

```rust
fn rendered_by_rust() -> &'static str {{
    "Emacs only inserts prepared line sexps"
}}
```

Display math:

$$
\int_0^1 x^2\,dx = \frac{{1}}{{3}}
$$

Open the tool artifact, resize the view with `/view 120`, or refresh the board with `g`.
"#
        )
    }

    fn split_response(text: &str) -> Vec<String> {
        let mut parts = Vec::new();
        let mut current = String::new();
        for line in text.lines() {
            current.push_str(line);
            current.push('\n');
            if current.len() > 180 {
                parts.push(std::mem::take(&mut current));
            }
        }
        if !current.is_empty() {
            parts.push(current);
        }
        parts
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn config_points_custom_provider_at_base_url() {
            let root = DemoRoot::new().unwrap();
            let config = root.path().join("config");
            fs::create_dir_all(&config).unwrap();
            write_config(&config, "http://127.0.0.1:9999").unwrap();
            let text = fs::read_to_string(config.join("config.toml")).unwrap();
            assert!(text.contains("provider = \"demo-openai\""));
            assert!(text.contains("[custom_providers.demo-openai]"));
            assert!(text.contains("base_url = \"http://127.0.0.1:9999\""));
            assert!(text.contains("api_style = \"completions\""));
            assert!(!text.contains("accept_all = true"));
        }

        #[test]
        fn first_demo_worker_can_request_permissions() {
            assert_eq!(
                worker_permission_args(WorkerPermissions::Ask),
                ["--restrictive"]
            );
            assert_eq!(
                worker_permission_args(WorkerPermissions::AcceptAll),
                ["--accept-all"]
            );
        }

        #[test]
        fn emacs_eval_sends_prompt_to_returned_chat_buffer() {
            let eval = emacs_eval(
                Path::new("/bin/zerostack"),
                Some(Path::new("/tmp/zsd/sock")),
            );
            assert!(eval.contains("(let ((buf (zerostack-connect \"/tmp/zsd/sock\")))"));
            assert!(eval.contains("(buffer-live-p buf)"));
            assert!(eval.contains("(with-current-buffer buf"));
            assert!(eval.contains("project-local demo skills"));
            assert!(eval.contains("task subagent call"));
            assert!(eval.contains("inline permission buttons"));
            assert!(!eval.contains("get-buffer zerostack-buffer-name"));
        }

        #[test]
        fn demo_projects_seed_project_local_skills() {
            let root = DemoRoot::new().unwrap();
            let repo = root.path().join("repo");
            write_demo_skills(&repo).unwrap();

            let claude = fs::read_to_string(
                repo.join(".claude")
                    .join("skills")
                    .join("render-review")
                    .join("SKILL.md"),
            )
            .unwrap();
            let opencode = fs::read_to_string(
                repo.join(".opencode")
                    .join("skills")
                    .join("tool-tour")
                    .join("SKILL.md"),
            )
            .unwrap();

            assert!(claude.contains("name: render-review"));
            assert!(claude.contains("inline LaTeX SVGs"));
            assert!(opencode.contains("name: tool-tour"));
            assert!(opencode.contains("task subagents"));
            assert!(opencode.contains("write_todo_list"));
        }

        #[test]
        fn session_json_matches_zerostack_storage_shape() {
            let root = DemoRoot::new().unwrap();
            let env = DemoEnv {
                root: root.path().to_path_buf(),
                data: root.path().join("d"),
                runtime: root.path().join("r"),
                config: root.path().join("c"),
                lisp: root.path().join("l"),
                projects: root.path().join("p"),
                logs: root.path().join("log"),
                attachments: root.path().join("att"),
            };
            env.create_dirs().unwrap();
            let cwd = root.path().join("repo");
            fs::create_dir_all(&cwd).unwrap();
            let spec = seed_session(&env, "Test", &cwd, true, false, 0, "hello").unwrap();
            let json: Value = serde_json::from_str(
                &fs::read_to_string(env.data.join("sessions").join(format!("{}.json", spec.id)))
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(json["provider"], PROVIDER);
            assert_eq!(json["model"], MODEL);
            assert_eq!(json["messages"][0]["role"], "user");
            assert_eq!(json["permission_allowlist"].as_array().unwrap().len(), 0);
            assert!(json["working_dir"].as_str().unwrap().ends_with("repo"));
        }

        #[test]
        fn first_demo_session_allows_remaining_tools_after_one_permission() {
            let root = DemoRoot::new().unwrap();
            let env = DemoEnv {
                root: root.path().to_path_buf(),
                data: root.path().join("d"),
                runtime: root.path().join("r"),
                config: root.path().join("c"),
                lisp: root.path().join("l"),
                projects: root.path().join("p"),
                logs: root.path().join("log"),
                attachments: root.path().join("att"),
            };
            env.create_dirs().unwrap();
            let cwd = root.path().join("repo");
            fs::create_dir_all(&cwd).unwrap();
            let spec = seed_session(&env, "Test", &cwd, true, true, 0, "hello").unwrap();
            let json: Value = serde_json::from_str(
                &fs::read_to_string(env.data.join("sessions").join(format!("{}.json", spec.id)))
                    .unwrap(),
            )
            .unwrap();
            let entries = json["permission_allowlist"].as_array().unwrap();
            let tools = entries
                .iter()
                .map(|entry| entry["tool"].as_str().unwrap())
                .collect::<Vec<_>>();

            assert!(!tools.contains(&"read"));
            for expected in [
                "list_dir",
                "find_files",
                "grep",
                "task",
                "write",
                "edit",
                "bash",
                "write_todo_list",
            ] {
                assert!(tools.contains(&expected), "missing {expected}");
            }
        }

        #[test]
        fn demo_projects_seed_huge_many_and_nongit_sessions() {
            let root = DemoRoot::new().unwrap();
            let env = DemoEnv {
                root: root.path().to_path_buf(),
                data: root.path().join("d"),
                runtime: root.path().join("r"),
                config: root.path().join("c"),
                lisp: root.path().join("l"),
                projects: root.path().join("p"),
                logs: root.path().join("log"),
                attachments: root.path().join("att"),
            };
            env.create_dirs().unwrap();
            let specs = create_demo_projects_and_sessions(&env).unwrap();
            let session_dir = env.data.join("sessions");
            let mut huge_messages = 0;
            let mut crowded = 0;
            let mut nongit = 0;

            for spec in specs {
                let json: Value = serde_json::from_str(
                    &fs::read_to_string(session_dir.join(format!("{}.json", spec.id))).unwrap(),
                )
                .unwrap();
                let title = json["name"].as_str().unwrap();
                if title == "Huge transcript stress test" {
                    huge_messages = json["messages"].as_array().unwrap().len();
                }
                if json["working_dir"]
                    .as_str()
                    .unwrap()
                    .contains("alpha-engine-tables")
                {
                    crowded += 1;
                }
                if json["working_dir"].as_str().unwrap().contains("nongit") {
                    nongit += 1;
                }
            }

            assert!(huge_messages > 100);
            assert!(crowded > 10);
            assert!(nongit >= 3);
        }

        #[test]
        fn provider_streams_reasoning_tool_call_and_final_markdown() {
            let request = json!({
                "messages": [{ "role": "user", "content": "demo" }],
                "tools": [{ "type": "function", "function": { "name": "read" } }]
            });
            let sse = chat_completion_sse(&request, 7);
            assert!(sse.contains("reasoning_content"));
            assert!(sse.contains("reasoning effort is default"));
            assert!(sse.contains("tool_calls"));
            assert!(sse.contains(r#""name":"read""#));
            assert!(sse.contains(r#""finish_reason":"tool_calls""#));

            let request_after_tool = json!({
                "messages": [
                    { "role": "user", "content": "demo" },
                    { "role": "tool", "content": "README" }
                ],
                "tools": [{ "type": "function", "function": { "name": "read" } }]
            });
            let final_sse = chat_completion_sse(&request_after_tool, 8);
            assert!(final_sse.contains("Zerostack Emacs demo"));
            assert!(final_sse.contains("$E = mc^2$"));
            assert!(final_sse.contains("data: [DONE]"));
        }

        #[test]
        fn provider_acknowledges_requested_reasoning_effort() {
            let request = json!({
                "messages": [{ "role": "user", "content": "demo" }],
                "reasoning": { "effort": "high" }
            });
            let sse = chat_completion_sse(&request, 9);
            assert!(sse.contains("reasoning effort is high"));
            assert!(sse.contains("acknowledged reasoning effort high"));
        }

        #[test]
        fn provider_saves_attachments_and_skips_tool_loop() {
            let root = DemoRoot::new().unwrap();
            let attachment_dir = root.path().join("attachments");
            let encoded = BASE64_STANDARD.encode(b"demo image bytes");
            let request = json!({
                "messages": [{
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "what did I attach?" },
                        {
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:image/png;base64,{encoded}")
                            }
                        }
                    ]
                }],
                "tools": [{ "type": "function", "function": { "name": "read" } }]
            });

            let frames =
                chat_completion_frames_with_attachments(&request, 3, &attachment_dir).unwrap();
            let sse = frames.concat();
            let saved_path = attachment_dir.join("request-0003-attachment-01.png");

            assert!(saved_path.is_file());
            assert_eq!(fs::read(&saved_path).unwrap(), b"demo image bytes");
            assert!(sse.contains("Demo attachments received"));
            assert!(sse.contains(&saved_path.display().to_string()));
            assert!(!sse.contains("tool_calls"));
            assert!(sse.contains("data: [DONE]"));
        }

        #[test]
        fn provider_walks_all_advertised_demo_tools_before_final_answer() {
            let tools = DEMO_TOOL_SEQUENCE
                .iter()
                .map(|name| json!({ "type": "function", "function": { "name": name } }))
                .collect::<Vec<_>>();
            let mut messages = vec![json!({ "role": "user", "content": "demo" })];

            for (idx, expected) in DEMO_TOOL_SEQUENCE.iter().enumerate() {
                let request = json!({ "messages": messages.clone(), "tools": tools.clone() });
                assert_eq!(choose_tool(&request), Some(*expected));
                messages = request["messages"].as_array().unwrap().clone();
                messages.push(json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "id": format!("call_{idx}"),
                        "type": "function",
                        "function": { "name": expected, "arguments": "{}" }
                    }]
                }));
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": format!("call_{idx}"),
                    "content": "ok"
                }));
            }

            let request = json!({ "messages": messages, "tools": tools });
            assert_eq!(choose_tool(&request), None);
            assert!(chat_completion_sse(&request, 99).contains("Zerostack Emacs demo"));
        }

        #[test]
        fn provider_reads_saved_tool_output_after_normal_tool_tour() {
            let tools = DEMO_TOOL_SEQUENCE
                .iter()
                .map(|name| json!({ "type": "function", "function": { "name": name } }))
                .collect::<Vec<_>>();
            let saved_path = "/tmp/zs/d/tool-outputs/session-id/0000-bash.txt";
            let saved_notice = format!(
                "bash:\nhead\n\n[tool output truncated: 14000 characters; 4000 omitted]\n[full output saved to: {saved_path}; use the read tool on this path to inspect the complete output]\n\ntail"
            );
            let mut messages = vec![json!({ "role": "user", "content": "demo" })];

            for (idx, expected) in DEMO_TOOL_SEQUENCE.iter().enumerate() {
                let request = json!({ "messages": messages.clone(), "tools": tools.clone() });
                assert_eq!(choose_tool(&request), Some(*expected));
                messages.push(json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "id": format!("call_{idx}"),
                        "type": "function",
                        "function": { "name": expected, "arguments": "{}" }
                    }]
                }));
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": format!("call_{idx}"),
                    "content": if *expected == "bash" { saved_notice.clone() } else { "ok".to_string() }
                }));
            }

            let request = json!({ "messages": messages.clone(), "tools": tools.clone() });
            assert_eq!(choose_tool(&request), Some("read"));
            let args: Value = serde_json::from_str(&demo_tool_arguments("read", &request)).unwrap();
            assert_eq!(args["path"], saved_path);

            let read_args = json!({ "path": saved_path, "offset": 1, "limit": 80 }).to_string();
            messages.push(json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_sidecar_read",
                    "type": "function",
                    "function": { "name": "read", "arguments": read_args }
                }]
            }));
            messages.push(json!({
                "role": "tool",
                "tool_call_id": "call_sidecar_read",
                "content": "demo sidecar line 000"
            }));

            let request_after_read = json!({ "messages": messages, "tools": tools });
            assert_eq!(choose_tool(&request_after_read), None);
        }

        #[test]
        fn demo_bash_tool_generates_sidecar_sized_output() {
            let request = json!({ "messages": [{ "role": "user", "content": "demo" }] });
            let first_args: Value =
                serde_json::from_str(&demo_tool_arguments("bash", &request)).unwrap();
            let first_command = first_args["command"].as_str().unwrap();

            assert!(first_command.contains("RTK demo command"));
            assert!(first_args.get("disable_rtk").is_none());

            let second_request = json!({
                "messages": [
                    { "role": "user", "content": "demo" },
                    {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call_bash_1",
                            "type": "function",
                            "function": { "name": "bash", "arguments": first_args.to_string() }
                        }]
                    },
                    { "role": "tool", "tool_call_id": "call_bash_1", "content": "ok" }
                ]
            });
            let args: Value =
                serde_json::from_str(&demo_tool_arguments("bash", &second_request)).unwrap();
            let command = args["command"].as_str().unwrap();

            assert_eq!(args["disable_rtk"], true);
            assert_eq!(args["timeout"], 45000);
            assert!(command.contains("demo live output line"));
            assert!(command.contains("Raw live-output demo"));
            assert!(command.contains("-lt 260"));
            assert!(command.contains("sleep 0.12"));
        }

        #[test]
        fn demo_task_arguments_request_a_subagent_prompt() {
            let request = json!({ "messages": [{ "role": "user", "content": "demo" }] });
            let args: Value = serde_json::from_str(&demo_tool_arguments("task", &request)).unwrap();
            let prompts = args["prompts"].as_array().unwrap();
            assert_eq!(prompts.len(), 1);
            assert!(prompts[0].as_str().unwrap().contains("demo workspace"));
        }

        #[test]
        fn demo_write_paths_are_unique_after_aborted_prompt_history() {
            let first_prompt = json!({
                "messages": [{ "role": "user", "content": "first demo" }]
            });
            let second_prompt_after_abort = json!({
                "messages": [
                    { "role": "user", "content": "first demo" },
                    { "role": "user", "content": "retry demo" }
                ]
            });

            let first_args: Value =
                serde_json::from_str(&demo_tool_arguments("write", &first_prompt)).unwrap();
            let second_args: Value =
                serde_json::from_str(&demo_tool_arguments("write", &second_prompt_after_abort))
                    .unwrap();

            assert_eq!(first_args["path"], "demo-output/generated-1.md");
            assert_eq!(second_args["path"], "demo-output/generated-2.md");
        }

        #[test]
        fn http_provider_serves_models_and_chat() {
            let provider = ProviderServer::start_with_delay(Duration::ZERO).unwrap();
            let models = raw_http(
                provider.addr,
                "GET /models HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            );
            assert!(models.contains(MODEL));

            let body = json!({
                "messages": [{ "role": "user", "content": "demo" }],
                "tools": [{ "type": "function", "function": { "name": "read" } }]
            })
            .to_string();
            let request = format!(
                "POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let chat = raw_http(provider.addr, &request);
            assert!(chat.contains("text/event-stream"));
            assert!(chat.contains("tool_calls"));
        }

        #[test]
        fn http_provider_can_demo_one_transient_overload() {
            let provider =
                ProviderServer::start_with_transient_failures(Duration::ZERO, 1).unwrap();
            let body = json!({ "messages": [{ "role": "user", "content": "demo" }] }).to_string();
            let request = format!(
                "POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );

            let first = raw_http(provider.addr, &request);
            assert!(first.contains("503 Service Unavailable"));
            assert!(first.contains("demo provider overloaded"));

            let second = raw_http(provider.addr, &request);
            assert!(second.contains("text/event-stream"));
            assert!(second.contains("Zerostack Emacs demo"));
        }

        fn raw_http(addr: SocketAddr, request: &str) -> String {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        }
    }
}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    unix_demo::run()
}
