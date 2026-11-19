use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use compact_str::CompactString;
use rig::tool::Tool;
use serde::Deserialize;
use tokio::process::Command;
use uuid::Uuid;

use crate::agent::tools::{ToolError, check_perm};
use crate::event::AgentEvent;
use crate::extras::subagents::{clone_subagent_event_tx, with_config, workspace};
use crate::extras::truncate::truncate_cjk;
use crate::permission::ask::AskSender;
use crate::permission::checker::PermCheck;

const MAX_SUBAGENT_RESPONSE_BYTES: usize = 128 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    Read,
    Write,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SpawnRequest {
    pub task: String,
    pub session_id: Option<String>,
    pub access: Access,
    #[serde(default)]
    pub worktree: bool,
    pub timeout: u64,
    pub model: Option<String>,
    pub reasoning: Option<String>,
}

pub struct TaskTool {
    permission: Option<PermCheck>,
    ask_tx: Option<AskSender>,
}

impl TaskTool {
    pub fn new(permission: Option<PermCheck>, ask_tx: Option<AskSender>) -> Self {
        Self { permission, ask_tx }
    }
}

impl Tool for TaskTool {
    const NAME: &'static str = "task";
    type Error = ToolError;
    crate::agent::tools::expose_tool_errors!();
    type Args = SpawnRequest;
    type Output = String;

    fn description(&self) -> String {
        "Run a task in a separate zerostack process. Omit session_id for a new subagent, or pass a returned session ID to continue a finished subagent with its saved conversation and workspace. Reuse is limited to this parent's children and preserves their access level and model. Both access modes use the current workspace by default for new subagents. For write access, set worktree to true to create a persistent copy-on-write workspace containing all in-progress edits. Shared-workspace writes affect the parent directly; coordinate concurrent edits. Results include the response, session ID, transcript, workspace, and socket paths. The soft deadline interrupts active work and starts one tool-free final turn without a hard timeout."
                .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        let model_options = with_config(|cfg| {
            cfg.model_options
                .iter()
                .map(|option| option.name.clone())
                .collect::<Vec<_>>()
        });
        let default_model = model_options.first().cloned().unwrap_or_default();
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "Standalone task for the subagent." },
                "session_id": { "type": "string", "description": "Optional full session ID returned by an earlier task call. Continue that finished child with its saved conversation and workspace. Keep the same access and model. Omit for a new child." },
                "access": { "type": "string", "enum": ["read", "write"], "description": "Filesystem access level." },
                "worktree": { "type": "boolean", "default": false, "description": "For a new write subagent, create an isolated persistent Git worktree (or directory copy outside Git). Defaults to false: use the current workspace. Ignored for read access and session reuse." },
                "timeout": { "type": "integer", "minimum": 1, "description": "Soft deadline in seconds. When elapsed, active work is interrupted and exactly one tool-free final turn begins without a hard timeout." },
                "model": {
                    "type": "string",
                    "enum": model_options,
                    "description": format!("Optional permitted subagent model. For a new child, omit to use the default: {default_model}. For reuse, omit to keep the saved model; changing it is not supported.")
                },
                "reasoning": { "type": "string", "enum": ["off", "none", "minimal", "low", "medium", "high", "xhigh", "max"], "description": "Optional reasoning effort." }
            },
            "required": ["task", "access", "timeout"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::tool::ToolContext,
        mut args: SpawnRequest,
    ) -> Result<String, ToolError> {
        if !crate::extras::subagents::is_enabled() {
            return Err(ToolError::Msg(
                "task: subagents are disabled for this session".into(),
            ));
        }
        if args.task.trim().is_empty() {
            return Err(ToolError::Msg("task: task must not be empty".into()));
        }
        if args.timeout == 0 {
            return Err(ToolError::Msg(
                "task: timeout must be at least 1 second".into(),
            ));
        }
        check_perm(&self.permission, &self.ask_tx, Self::NAME, &args.task).await?;

        let source = std::env::current_dir()
            .map_err(|e| ToolError::Msg(format!("task: failed to get current directory: {e}")))?;
        let (parent, models) =
            with_config(|cfg| (cfg.parent_session_id.clone(), cfg.model_options.clone()));
        let request = args.clone();
        let prepared =
            tokio::task::spawn_blocking(move || prepare_task(&request, &parent, &models, &source))
                .await
                .map_err(|e| {
                    ToolError::Msg(format!("task: workspace preparation panicked: {e}"))
                })??;
        let session_id = prepared.session_id.as_str();
        let workspace_path = prepared.workspace.as_path();
        args.model = Some(prepared.model_name.clone());
        #[cfg(unix)]
        if tokio::net::UnixStream::connect(crate::extras::emacs::session_socket_path(session_id))
            .await
            .is_ok()
        {
            return Err(ToolError::Msg(
                "task: subagent session is already running".into(),
            ));
        }
        let transcript_path = crate::session::storage::session_path(session_id);
        allow_parent_access(
            &self.permission,
            args.access,
            workspace_path,
            &transcript_path,
        );

        let finalize_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .saturating_add(finalize_after_ms(args.timeout) as u128)
            .min(u64::MAX as u128) as u64;
        let (mut command, selected_model) =
            child_command(&args, session_id, workspace_path, finalize_at_unix_ms)?;
        let mut child = command
            .spawn()
            .map_err(|e| ToolError::Msg(format!("task: failed to start subagent process: {e}")))?;
        let mut process_guard = ProcessGroupGuard::new(child.id());
        let timeout = Duration::from_secs(args.timeout);
        let socket_path = crate::extras::emacs::session_socket_path(session_id);
        let socket_ready = wait_for_socket(
            &mut child,
            &socket_path,
            timeout.min(Duration::from_secs(5)),
        )
        .await;
        let event_tx = clone_subagent_event_tx();
        if let Some(tx) = event_tx {
            let _ = tx
                .send(AgentEvent::SubagentToolCall {
                    name: CompactString::new("session"),
                    args: serde_json::json!({
                        "session_id": session_id,
                        "workspace": workspace_path,
                        "socket": socket_ready.then(|| socket_path.clone()),
                        "model": selected_model.name,
                        "provider": selected_model.provider,
                        "resolved_model": selected_model.model,
                        "reasoning": args.reasoning.as_deref().unwrap_or("default"),
                        "access": access_name(args.access),
                        "timeout": args.timeout,
                    }),
                })
                .await;
        }

        let output = child.wait_with_output().await;
        if output.is_ok() {
            process_guard.disarm();
        }
        let response = match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                format!(
                    "[error: subagent process exited with {}: {}]",
                    output.status,
                    stderr.trim()
                )
            }
            Err(e) => format!("[error: failed to run subagent process: {e}]"),
        };
        let response = truncate_cjk(
            &response,
            MAX_SUBAGENT_RESPONSE_BYTES,
            &format!(
                "\n…[subagent response truncated at {}B]",
                MAX_SUBAGENT_RESPONSE_BYTES
            ),
        );
        Ok(format!(
            "{}\n\nSession: {}\nWorkspace: {}\nTranscript: {}\nSocket: {}\n",
            response,
            session_id,
            workspace_path.display(),
            transcript_path.display(),
            socket_path.display()
        ))
    }
}

pub(crate) struct PreparedTask {
    pub session_id: String,
    pub workspace: std::path::PathBuf,
    pub model_name: String,
    // Keep the lock until the child exits, including its final summary turn.
    _lock: std::fs::File,
}

pub(crate) fn prepare_task(
    args: &SpawnRequest,
    parent: &str,
    models: &[crate::extras::subagents::ModelOption],
    source: &Path,
) -> Result<PreparedTask, ToolError> {
    let session_id = match args.session_id.as_deref() {
        Some(id) => Uuid::parse_str(id)
            .map_err(|_| ToolError::Msg("task: session_id must be a full UUID".into()))?
            .to_string(),
        None => Uuid::new_v4().to_string(),
    };
    let directory = crate::session::storage::data_dir()
        .join("subagents")
        .join(&session_id);
    std::fs::create_dir_all(&directory)
        .map_err(|e| ToolError::Msg(format!("task: failed to create session directory: {e}")))?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("run.lock"))
        .map_err(|e| ToolError::Msg(format!("task: failed to open session lock: {e}")))?;
    lock.try_lock().map_err(|e| {
        ToolError::Msg(format!(
            "task: subagent session is already running or cannot be locked: {e}"
        ))
    })?;

    let (workspace, selected) = if args.session_id.is_some() {
        let json = std::fs::read(crate::session::storage::session_path(&session_id))
            .map_err(|e| ToolError::Msg(format!("task: cannot load subagent session: {e}")))?;
        let session: crate::session::Session = serde_json::from_slice(&json)
            .map_err(|e| ToolError::Msg(format!("task: invalid subagent session: {e}")))?;
        if session.id.as_str() != session_id || session.parent_session_id.as_deref() != Some(parent)
        {
            return Err(ToolError::Msg(
                "task: session does not belong to this parent".into(),
            ));
        }
        if session.subagent_access.as_deref() != Some(access_name(args.access)) {
            return Err(ToolError::Msg(
                "task: reuse must keep the subagent's access level".into(),
            ));
        }
        let selected = match args.model.as_deref() {
            Some(name) => select_model_option(models, Some(name))?,
            None => models
                .iter()
                .find(|option| {
                    option.provider == session.provider.as_str()
                        && option.model == session.model.as_str()
                })
                .ok_or_else(|| {
                    ToolError::Msg("task: saved subagent model is no longer permitted".into())
                })?,
        };
        if selected.provider != session.provider.as_str()
            || selected.model != session.model.as_str()
        {
            return Err(ToolError::Msg(
                "task: reuse must keep the subagent's model and provider".into(),
            ));
        }
        let workspace = Path::new(session.working_dir.as_str())
            .canonicalize()
            .map_err(|e| ToolError::Msg(format!("task: saved workspace is unavailable: {e}")))?;
        if !workspace.is_dir() {
            return Err(ToolError::Msg(
                "task: saved workspace is not a directory".into(),
            ));
        }
        (workspace, selected)
    } else {
        let selected = select_model_option(models, args.model.as_deref())?;
        (
            prepare_workspace(args.access, args.worktree, source, &session_id)?,
            selected,
        )
    };
    Ok(PreparedTask {
        session_id,
        workspace,
        model_name: selected.name.clone(),
        _lock: lock,
    })
}

pub(crate) fn prepare_workspace(
    access: Access,
    worktree: bool,
    source: &Path,
    session_id: &str,
) -> Result<std::path::PathBuf, ToolError> {
    if access == Access::Write && worktree {
        workspace::create(source, session_id).map_err(|e| ToolError::Msg(format!("task: {e}")))
    } else {
        source
            .canonicalize()
            .map_err(|e| ToolError::Msg(format!("task: failed to resolve workspace: {e}")))
    }
}

fn child_command(
    args: &SpawnRequest,
    session_id: &str,
    workspace: &Path,
    finalize_at_unix_ms: u64,
) -> Result<(Command, crate::extras::subagents::ModelOption), ToolError> {
    let (model_options, max_turns, parent_session_id) = with_config(|cfg| {
        (
            cfg.model_options.clone(),
            cfg.max_turns,
            cfg.parent_session_id.clone(),
        )
    });
    let selected = select_model_option(&model_options, args.model.as_deref())?;
    let executable = subagent_executable();
    let mut command = Command::new(executable);
    command
        .current_dir(workspace)
        .arg("--print")
        .arg("--provider")
        .arg(&selected.provider)
        .arg("--model")
        .arg(&selected.model)
        .arg("--max-agent-turns")
        .arg(max_turns.to_string())
        .arg("--subagent-session-id")
        .arg(session_id)
        .arg("--subagent-parent-session")
        .arg(parent_session_id)
        .arg("--subagent-access")
        .arg(match args.access {
            Access::Read => "read",
            Access::Write => "write",
        })
        .arg("--subagent-live")
        .arg("--subagent-finalize-at-unix-ms")
        .arg(finalize_at_unix_ms.to_string());
    if args.session_id.is_some() {
        command.arg("--session").arg(session_id);
    }
    match args.access {
        Access::Read => {
            command.arg("--read-only");
        }
        Access::Write => {
            command.args(["--accept-all", "--sandbox"]);
        }
    }
    if let Some(reasoning) = &args.reasoning {
        command.arg("--reasoning-effort").arg(reasoning);
    }
    let prompt = match args.access {
        Access::Read => format!(
            "Work as a read-only subagent. Investigate and report a concise, verified answer. Do not modify files.\n\n{}",
            args.task
        ),
        Access::Write if args.session_id.is_some() => format!(
            "Continue as a write subagent in your saved workspace, using the conversation history. Other agents may edit files here; preserve their work and limit edits to your assigned task. Complete the follow-up task, test the result, and report what changed.\n\n{}",
            args.task
        ),
        Access::Write if !args.worktree => format!(
            "Work as a write subagent in the parent's shared workspace. Changes affect the parent directly. Other agents may edit files here; preserve their work and limit edits to your assigned task. Complete the task, test the result, and report what changed.\n\n{}",
            args.task
        ),
        Access::Write => format!(
            "Work as an isolated write subagent. Complete the task in this workspace, test the result, and report what changed.\n\n{}",
            args.task
        ),
    };
    command
        .arg("--")
        .arg(prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    Ok((command, selected.clone()))
}

fn subagent_executable() -> std::path::PathBuf {
    resolve_subagent_executable(std::env::current_exe().ok())
}

pub(crate) fn resolve_subagent_executable(
    current: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    current
        .filter(|path| path.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("zerostack"))
}

pub(crate) struct ProcessGroupGuard {
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    pub(crate) fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            kill_process_group(pid);
        }
    }
}

pub(crate) fn finalize_after_ms(timeout_secs: u64) -> u64 {
    timeout_secs.saturating_mul(1_000)
}

fn access_name(access: Access) -> &'static str {
    match access {
        Access::Read => "read",
        Access::Write => "write",
    }
}

async fn wait_for_socket(
    child: &mut tokio::process::Child,
    socket: &Path,
    duration: Duration,
) -> bool {
    let deadline = Instant::now() + duration;
    loop {
        if socket.exists() {
            return true;
        }
        if child.try_wait().ok().flatten().is_some() || Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(crate) fn select_model_option<'a>(
    options: &'a [crate::extras::subagents::ModelOption],
    requested: Option<&str>,
) -> Result<&'a crate::extras::subagents::ModelOption, ToolError> {
    match requested {
        Some(name) => options.iter().find(|option| option.name == name),
        None => options.first(),
    }
    .ok_or_else(|| {
        let permitted = options
            .iter()
            .map(|option| option.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        ToolError::Msg(format!(
            "task: model '{}' is not permitted; choose one of: {}",
            requested.unwrap_or(""),
            permitted
        ))
    })
}

pub(crate) fn parent_workspace_tools(access: Access) -> &'static [&'static str] {
    match access {
        Access::Read => &["read", "list_dir"],
        Access::Write => &["read", "write", "edit", "list_dir"],
    }
}

fn allow_parent_access(
    permission: &Option<PermCheck>,
    access: Access,
    workspace: &Path,
    transcript: &Path,
) {
    let Some(permission) = permission else {
        return;
    };
    let mut permission = permission.lock().unwrap_or_else(|e| e.into_inner());
    let workspace = workspace.to_string_lossy();
    for tool in parent_workspace_tools(access) {
        permission.add_session_allowlist((*tool).to_string(), &workspace);
        permission.add_session_allowlist((*tool).to_string(), &format!("{workspace}/**"));
    }
    permission.add_session_allowlist("read".to_string(), &transcript.to_string_lossy());
}

fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        let group = format!("-{pid}");
        let _ = std::process::Command::new("kill")
            .args(["-TERM", "--", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &group])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
