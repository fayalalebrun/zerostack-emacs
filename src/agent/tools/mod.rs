pub(crate) mod bash;
pub(crate) mod crc;
pub(crate) mod edit;
pub(crate) mod find_files;
pub(crate) mod goal;
pub(crate) mod grep;
pub(crate) mod list_dir;
pub(crate) mod normalize;
pub(crate) mod read;
pub(crate) mod todo;
pub(crate) mod write;

pub(crate) use normalize::{levenshtein_similarity, normalize_whitespace};

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use crate::config::types::EditSystem;

static EDIT_SYSTEM: Mutex<EditSystem> = Mutex::new(EditSystem::Similarity);

pub(crate) fn set_edit_system(es: EditSystem) {
    *EDIT_SYSTEM.lock().unwrap_or_else(|e| e.into_inner()) = es;
}

pub(crate) fn edit_system() -> EditSystem {
    *EDIT_SYSTEM.lock().unwrap_or_else(|e| e.into_inner())
}

static DENY_REPEATED_READS: Mutex<bool> = Mutex::new(true);

pub(crate) fn set_deny_repeated_reads(v: bool) {
    *DENY_REPEATED_READS
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = v;
}

pub(crate) fn deny_repeated_reads() -> bool {
    *DENY_REPEATED_READS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

static READ_TRACKER: Mutex<Vec<(String, usize, usize)>> = Mutex::new(Vec::new());
static ACTIVE_SESSION_ID: Mutex<Option<String>> = Mutex::new(None);
static READ_LOADED_CONTEXT: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static READ_CONTEXT_METADATA: LazyLock<Mutex<HashMap<u64, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) type ContextTracker = Arc<Mutex<HashSet<PathBuf>>>;

pub(crate) fn new_context_tracker(paths: impl IntoIterator<Item = PathBuf>) -> ContextTracker {
    Arc::new(Mutex::new(paths.into_iter().collect()))
}

fn output_key(output: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    output.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn read_loaded_context() -> HashSet<PathBuf> {
    READ_LOADED_CONTEXT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub(crate) fn mark_read_context_loaded(paths: &[PathBuf]) {
    let mut loaded = READ_LOADED_CONTEXT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    loaded.extend(paths.iter().cloned());
}

pub(crate) fn reset_read_context_loaded(paths: impl IntoIterator<Item = PathBuf>) {
    let mut loaded = READ_LOADED_CONTEXT
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    loaded.clear();
    loaded.extend(paths);
}

pub(crate) fn loaded_context_from(tracker: &Option<ContextTracker>) -> HashSet<PathBuf> {
    tracker
        .as_ref()
        .map(|t| t.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .unwrap_or_else(read_loaded_context)
}

pub(crate) fn mark_context_loaded_in(tracker: &Option<ContextTracker>, paths: &[PathBuf]) {
    if let Some(t) = tracker {
        t.lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(paths.iter().cloned());
    } else {
        mark_read_context_loaded(paths);
    }
}

pub(crate) fn format_context_reminder(loaded: &[(PathBuf, String)]) -> Option<String> {
    if loaded.is_empty() {
        None
    } else {
        Some(format!(
            "<system-reminder>\n{}\n</system-reminder>",
            loaded
                .iter()
                .map(|(_, content)| content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        ))
    }
}

pub(crate) fn set_active_session_id(id: impl Into<Option<String>>) {
    *ACTIVE_SESSION_ID.lock().unwrap_or_else(|e| e.into_inner()) = id.into();
}

pub(crate) fn truncate_live_tool_output(tool_name: &str, output: &str) -> String {
    let output_chars = output.chars().count();
    if output_chars <= crate::session::TOOL_RESULT_SAVE_THRESHOLD {
        return output.to_string();
    }

    let head: String = output
        .chars()
        .take(crate::session::TOOL_RESULT_HEAD_CHARS)
        .collect();
    let tail: String = output
        .chars()
        .skip(output_chars.saturating_sub(crate::session::TOOL_RESULT_TAIL_CHARS))
        .collect();
    let omitted = output_chars.saturating_sub(
        crate::session::TOOL_RESULT_HEAD_CHARS + crate::session::TOOL_RESULT_TAIL_CHARS,
    );
    let saved = ACTIVE_SESSION_ID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .and_then(|id| crate::session::storage::save_tool_output(&id, tool_name, output).ok())
        .map(|path| {
            format!(
                "\n[full output saved to: {}; use the read tool on this path to inspect the complete output]",
                path.display()
            )
        })
        .unwrap_or_else(|| {
            "\n[full output was not saved; rerun a narrower command if more detail is needed]"
                .to_string()
        });

    format!(
        "{head}\n\n[tool output truncated for live model context: {output_chars} characters; {omitted} omitted]{saved}\n\n{tail}"
    )
}

pub(crate) fn register_read_context_metadata(output: &str, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    READ_CONTEXT_METADATA
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(output_key(output), paths);
}

pub(crate) fn take_read_context_metadata(output: &str) -> Vec<String> {
    READ_CONTEXT_METADATA
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&output_key(output))
        .unwrap_or_default()
}

pub(crate) fn track_read(path: &str, offset: usize, limit: usize) -> Option<String> {
    if !deny_repeated_reads() {
        return None;
    }
    let mut tracker = READ_TRACKER.lock().unwrap_or_else(|e| e.into_inner());
    let key = (path.to_string(), offset, limit);
    if tracker.contains(&key) {
        let end = (offset + limit).saturating_sub(1);
        Some(format!(
            "read blocked: {path} (lines {}-{}) was already read and has not been modified since. Use the previous result or read a different section.",
            offset + 1,
            if end > 0 { end } else { offset + 1 }
        ))
    } else {
        tracker.push(key);
        None
    }
}

pub(crate) fn untrack_read_path(path: &str) {
    let mut tracker = READ_TRACKER.lock().unwrap_or_else(|e| e.into_inner());
    tracker.retain(|(p, _, _)| p != path);
}

pub use bash::BashTool;
pub use edit::EditTool;
pub use find_files::FindFilesTool;
pub use goal::UpdateGoal;
pub use grep::GrepTool;
pub use list_dir::ListDirTool;
pub use read::ReadTool;
pub use todo::WriteTodoList;
pub use write::WriteTool;

use std::io;

use compact_str::CompactString;
use serde::Deserialize;

use crate::permission::ask::{AskRequest, AskSender, UserDecision};
use crate::permission::checker::{CheckResult, PermCheck};

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    Msg(String),
}

impl From<io::Error> for ToolError {
    fn from(e: io::Error) -> Self {
        ToolError::Msg(e.to_string())
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(e: serde_json::Error) -> Self {
        ToolError::Msg(e.to_string())
    }
}

pub fn is_skip_dir(name: &str) -> bool {
    matches!(name, "node_modules" | "target")
}

#[derive(Deserialize)]
pub struct ReadArgs {
    pub path: String,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct WriteArgs {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct EditArgs {
    pub path: String,
    #[serde(default)]
    pub block: Option<String>,
    #[serde(default)]
    pub file_crc: Option<String>,
    #[serde(default)]
    pub edits: Option<Vec<EditOp>>,
}

#[derive(Debug, Clone)]
pub(crate) struct EditBlock {
    pub search: String,
    pub replace: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct EditOp {
    pub line: Option<String>,
    pub lines: Option<String>,
    pub text: String,
}

#[derive(Deserialize)]
pub struct BashArgs {
    pub command: String,
    pub timeout: Option<u64>,
    #[cfg(feature = "rtk")]
    pub disable_rtk: Option<bool>,
}

#[derive(Deserialize)]
pub struct GrepArgs {
    pub pattern: String,
    pub path: Option<String>,
    pub include: Option<String>,
    pub context_lines: Option<usize>,
}

#[derive(Deserialize)]
pub struct FindFilesArgs {
    pub pattern: String,
    pub path: Option<String>,
}

#[derive(Deserialize)]
pub struct ListDirArgs {
    pub path: Option<String>,
}

async fn handle_ask_inner(
    ask_tx: &AskSender,
    permission: &PermCheck,
    tool: &str,
    input: &str,
) -> Result<(), ToolError> {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    ask_tx
        .send(AskRequest {
            tool: CompactString::new(tool),
            input: input.to_string(),
            reply: reply_tx,
        })
        .await
        .map_err(|_| ToolError::Msg("Permission system unavailable".to_string()))?;
    match reply_rx.await {
        Ok(UserDecision::AllowOnce) => Ok(()),
        Ok(UserDecision::AllowAlways(pattern)) => {
            permission
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .add_session_allowlist(tool.to_string(), &pattern);
            Ok(())
        }
        _ => Err(ToolError::Msg("Permission denied by user".to_string())),
    }
}

pub async fn check_perm(
    permission: &Option<PermCheck>,
    ask_tx: &Option<AskSender>,
    tool: &str,
    input_key: &str,
) -> Result<Option<String>, ToolError> {
    let Some(perm) = permission else {
        return Ok(None);
    };
    let result = {
        let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
        guard.check(tool, input_key)
    };
    match result {
        CheckResult::Allowed => Ok(None),
        CheckResult::AllowedWithCoaching(msg) => Ok(Some(msg)),
        CheckResult::Denied(reason) => {
            Err(ToolError::Msg(format!("Permission denied: {}", reason)))
        }
        CheckResult::Ask => {
            let Some(tx) = ask_tx else {
                return Err(ToolError::Msg(
                    "Permission denied (non-interactive mode)".to_string(),
                ));
            };
            handle_ask_inner(tx, perm, tool, input_key).await?;
            Ok(None)
        }
    }
}

pub async fn check_perm_path(
    permission: &Option<PermCheck>,
    ask_tx: &Option<AskSender>,
    tool: &str,
    path: &str,
) -> Result<Option<String>, ToolError> {
    let Some(perm) = permission else {
        return Ok(None);
    };
    let result = {
        let mut guard = perm.lock().unwrap_or_else(|e| e.into_inner());
        guard.check_path(tool, path)
    };
    match result {
        CheckResult::Allowed => Ok(None),
        CheckResult::AllowedWithCoaching(msg) => Ok(Some(msg)),
        CheckResult::Denied(reason) => {
            Err(ToolError::Msg(format!("Permission denied: {}", reason)))
        }
        CheckResult::Ask => {
            let Some(tx) = ask_tx else {
                return Err(ToolError::Msg(
                    "Permission denied (non-interactive mode)".to_string(),
                ));
            };
            handle_ask_inner(tx, perm, tool, path).await?;
            Ok(None)
        }
    }
}
