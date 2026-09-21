use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use compact_str::CompactString;
use rig::completion::ToolDefinition;
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

#[derive(Debug, Deserialize)]
pub struct SpawnRequest {
    pub task: String,
    pub access: Access,
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
    type Args = SpawnRequest;
    type Output = String;

    async fn definition(&self, _p: String) -> ToolDefinition {
        let model_options = with_config(|cfg| {
            cfg.model_options
                .iter()
                .map(|option| option.name.clone())
                .collect::<Vec<_>>()
        });
        let default_model = model_options.first().cloned().unwrap_or_default();
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Spawn a fresh zerostack process for an isolated task. Read access uses the current workspace. Write access creates a persistent copy-on-write workspace containing all in-progress edits. The result includes the child response, session transcript, and workspace paths. The parent can access both paths. The soft deadline interrupts active work and starts one tool-free final turn; that final turn has no timeout."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "Standalone task for the subagent." },
                    "access": { "type": "string", "enum": ["read", "write"], "description": "Filesystem access level." },
                    "timeout": { "type": "integer", "minimum": 1, "description": "Soft deadline in seconds. When elapsed, active work is interrupted and exactly one tool-free final turn begins without a hard timeout." },
                    "model": {
                        "type": "string",
                        "enum": model_options,
                        "description": format!("Optional permitted subagent model. Omit to use the default: {default_model}.")
                    },
                    "reasoning": { "type": "string", "enum": ["off", "none", "minimal", "low", "medium", "high", "xhigh", "max"], "description": "Optional reasoning effort." }
                },
                "required": ["task", "access", "timeout"]
            }),
        }
    }

    async fn call(&self, args: SpawnRequest) -> Result<String, ToolError> {
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

        let session_id = Uuid::new_v4().to_string();
        let source = std::env::current_dir()
            .map_err(|e| ToolError::Msg(format!("task: failed to get current directory: {e}")))?;
        let workspace_path = match args.access {
            Access::Read => source
                .canonicalize()
                .map_err(|e| ToolError::Msg(format!("task: failed to resolve workspace: {e}")))?,
            Access::Write => {
                let source = source.clone();
                let id = session_id.clone();
                tokio::task::spawn_blocking(move || workspace::create(&source, &id))
                    .await
                    .map_err(|e| ToolError::Msg(format!("task: workspace creation panicked: {e}")))?
                    .map_err(|e| ToolError::Msg(format!("task: {e}")))?
            }
        };
        let transcript_path = crate::session::storage::session_path(&session_id);
        allow_parent_access(
            &self.permission,
            args.access,
            &workspace_path,
            &transcript_path,
        );

        let finalize_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .saturating_add(finalize_after_ms(args.timeout) as u128)
            .min(u64::MAX as u128) as u64;
        let (mut command, selected_model) =
            child_command(&args, &session_id, &workspace_path, finalize_at_unix_ms)?;
        let mut child = command
            .spawn()
            .map_err(|e| ToolError::Msg(format!("task: failed to start subagent process: {e}")))?;
        let mut process_guard = ProcessGroupGuard::new(child.id());
        let timeout = Duration::from_secs(args.timeout);
        let socket_path = crate::extras::emacs::session_socket_path(&session_id);
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
