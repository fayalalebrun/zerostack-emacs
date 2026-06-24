use std::path::PathBuf;
use std::process::Output;
use std::sync::Mutex;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, timeout};

use crate::agent::tools::{AskSender, BashArgs, PermCheck, ToolError, check_perm};
use crate::extras::truncate::head_lines;
use crate::sandbox::Sandbox;

pub(crate) struct BashLiveOutputRequest {
    pub command: String,
    pub reply: oneshot::Sender<Option<PathBuf>>,
}

pub(crate) type BashLiveOutputSender = mpsc::Sender<BashLiveOutputRequest>;

static BASH_LIVE_OUTPUT_TX: Mutex<Option<BashLiveOutputSender>> = Mutex::new(None);

pub(crate) fn set_bash_live_output_sender(sender: Option<BashLiveOutputSender>) {
    *BASH_LIVE_OUTPUT_TX
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = sender;
}

fn bash_live_output_sender() -> Option<BashLiveOutputSender> {
    BASH_LIVE_OUTPUT_TX
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub(crate) fn split_bash_commands(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            current.push(ch);
            if let Some(next) = chars.next() {
                current.push(next);
            }
        } else if ch == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            current.push(ch);
        } else if ch == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            current.push(ch);
        } else if ch == ';' && !in_single_quote && !in_double_quote {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                result.push(trimmed);
            }
            current = String::new();
        } else if ch == '&' && !in_single_quote && !in_double_quote {
            if chars.peek() == Some(&'&') {
                chars.next();
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    result.push(trimmed);
                }
                current = String::new();
            } else {
                current.push(ch);
            }
        } else if ch == '|' && !in_single_quote && !in_double_quote {
            if chars.peek() == Some(&'|') {
                chars.next();
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    result.push(trimmed);
                }
                current = String::new();
            } else {
                current.push(ch);
            }
        } else if ch == '>' && !in_single_quote && !in_double_quote {
            if chars.peek() == Some(&'>') {
                chars.next();
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    result.push(trimmed);
                }
                current = String::new();
            } else {
                current.push(ch);
            }
        } else {
            current.push(ch);
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        result.push(trimmed);
    }

    result
}

pub struct BashTool {
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
    pub sandbox: Sandbox,
    /// `None` = no truncation (matches the historical behaviour). `Some(n)`
    /// = head-only truncation after `n` lines with a recovery hint.
    pub max_output_lines: Option<u64>,
}

impl BashTool {
    pub fn new(
        permission: Option<PermCheck>,
        ask_tx: Option<AskSender>,
        sandbox: Sandbox,
        max_output_lines: Option<u64>,
    ) -> Self {
        BashTool {
            permission,
            ask_tx,
            sandbox,
            max_output_lines,
        }
    }

    async fn run_buffered_command(
        &self,
        command: &str,
        timeout_millis: Option<u64>,
    ) -> Result<Output, ToolError> {
        if let Some(secs) = timeout_millis {
            match timeout(
                Duration::from_millis(secs),
                self.sandbox.output_command(command),
            )
            .await
            {
                Ok(output) => Ok(output?),
                Err(_) => {
                    self.sandbox.kill_active();
                    Err(ToolError::Msg("Command timed out".to_string()))
                }
            }
        } else {
            Ok(self.sandbox.output_command(command).await?)
        }
    }
}

#[cfg(any(feature = "rtk", test))]
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(any(feature = "rtk", test))]
pub(crate) fn rtk_wrap_command(command: &str) -> String {
    let trimmed = command.trim_start();
    if trimmed == "rtk" || trimmed.starts_with("rtk ") {
        command.to_string()
    } else {
        format!("rtk bash -lc {}", shell_quote(command))
    }
}

#[cfg(any(feature = "rtk", test))]
pub(crate) fn rtk_command_for_call(command: &str, disable_rtk: bool) -> String {
    if disable_rtk {
        command.to_string()
    } else {
        rtk_wrap_command(command)
    }
}

impl Tool for BashTool {
    const NAME: &'static str = "bash";

    type Error = ToolError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        let properties = {
            #[cfg(feature = "rtk")]
            {
                let mut properties = serde_json::json!({
                    "command": { "type": "string", "description": "Bash command to execute" },
                    "timeout": { "type": "integer", "description": "Timeout in milliseconds (optional)" }
                });
                if let Some(properties) = properties.as_object_mut() {
                    properties.insert(
                        "disable_rtk".to_string(),
                        serde_json::json!({
                            "type": "boolean",
                            "description": "Set true to execute this command without RTK wrapping when raw output is needed"
                        }),
                    );
                }
                properties
            }
            #[cfg(not(feature = "rtk"))]
            {
                serde_json::json!({
                    "command": { "type": "string", "description": "Bash command to execute" },
                    "timeout": { "type": "integer", "description": "Timeout in milliseconds (optional)" }
                })
            }
        };

        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a bash command in the current working directory. Returns stdout and stderr.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: BashArgs) -> Result<String, ToolError> {
        let mut coaching: Option<String> = None;
        for cmd in split_bash_commands(&args.command) {
            if let Some(msg) = check_perm(&self.permission, &self.ask_tx, "bash", &cmd).await? {
                coaching = Some(msg);
            }
        }

        #[cfg(feature = "rtk")]
        let command = rtk_command_for_call(&args.command, args.disable_rtk.unwrap_or(false));
        #[cfg(not(feature = "rtk"))]
        let command = args.command.clone();

        let (stdout, stderr, exit_code) = if let Some(sender) = bash_live_output_sender() {
            let (reply, response) = oneshot::channel();
            let _ = sender
                .send(BashLiveOutputRequest {
                    command: args.command.clone(),
                    reply,
                })
                .await;
            if let Ok(Some(path)) = response.await {
                let status = if let Some(secs) = args.timeout {
                    match timeout(
                        Duration::from_millis(secs),
                        self.sandbox.output_command_to_file(&command, &path),
                    )
                    .await
                    {
                        Ok(status) => status,
                        Err(_) => {
                            self.sandbox.kill_active();
                            return Err(ToolError::Msg("Command timed out".to_string()));
                        }
                    }
                } else {
                    self.sandbox.output_command_to_file(&command, &path).await
                }?;
                let output = tokio::fs::read(&path).await?;
                (
                    String::from_utf8_lossy(&output).to_string(),
                    String::new(),
                    status.code().unwrap_or(-1),
                )
            } else {
                let output = self.run_buffered_command(&command, args.timeout).await?;
                (
                    String::from_utf8_lossy(&output.stdout).to_string(),
                    String::from_utf8_lossy(&output.stderr).to_string(),
                    output.status.code().unwrap_or(-1),
                )
            }
        } else {
            let output = self.run_buffered_command(&command, args.timeout).await?;
            (
                String::from_utf8_lossy(&output.stdout).to_string(),
                String::from_utf8_lossy(&output.stderr).to_string(),
                output.status.code().unwrap_or(-1),
            )
        };

        let mut result = String::new();
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&stderr);
        }
        if exit_code != 0 {
            result.push_str(&format!("\nExit code: {}", exit_code));
        }

        let result = if let Some(cap) = self.max_output_lines {
            let cap = cap as usize;
            let (head, total) = head_lines(&result, cap);
            if total > cap {
                format!(
                    "{}\n\n[truncated after {} lines — {} more lines elided; re-run with a narrower invocation or pipe through `tail`/`grep` to see trailing output]",
                    head,
                    cap,
                    total - cap,
                )
            } else {
                result
            }
        } else {
            result
        };

        let result = match coaching {
            Some(msg) => format!("{}\n\n{}", msg, result),
            None => result,
        };
        Ok(crate::agent::tools::truncate_live_tool_output(
            Self::NAME,
            &result,
        ))
    }
}
