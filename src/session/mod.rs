pub mod chat_history;
pub mod storage;

use std::path::{Path, PathBuf};

use compact_str::CompactString;
use rig::OneOrMany;
use rig::completion::message::{AssistantContent, Reasoning, ReasoningContent, Text};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TOOL_RESULT_SAVE_THRESHOLD: usize = 12_000;
pub const TOOL_RESULT_HEAD_CHARS: usize = 2_000;
pub const TOOL_RESULT_TAIL_CHARS: usize = 8_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    ToolCall,
    ToolResult,
    SubagentToolCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: MessageRole,
    pub content: CompactString,
    pub estimated_tokens: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_reasoning: Vec<ProviderReasoning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_usage: Option<SessionTokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<SessionToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<SessionToolResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionToolCall {
    pub id: CompactString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<CompactString>,
    pub name: CompactString,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolResult {
    pub id: CompactString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<CompactString>,
    pub name: CompactString,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loaded_context: Vec<CompactString>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl SessionTokenUsage {
    pub fn context_tokens(self) -> u64 {
        if self.total_tokens > 0 {
            return self.total_tokens;
        }
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }
}

impl From<crate::event::TokenUsage> for SessionTokenUsage {
    fn from(usage: crate::event::TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
pub enum ProviderReasoningContent {
    Summary(String),
    Encrypted(String),
    Redacted(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderReasoning {
    pub id: String,
    pub content: Vec<ProviderReasoningContent>,
}

impl ProviderReasoning {
    pub fn from_rig(reasoning: &Reasoning) -> Option<Self> {
        let id = reasoning.id.clone()?;
        let content = reasoning
            .content
            .iter()
            .filter_map(|item| match item {
                ReasoningContent::Summary(text) => {
                    Some(ProviderReasoningContent::Summary(text.clone()))
                }
                ReasoningContent::Encrypted(data) => {
                    Some(ProviderReasoningContent::Encrypted(data.clone()))
                }
                ReasoningContent::Redacted { data } => {
                    Some(ProviderReasoningContent::Redacted(data.clone()))
                }
                ReasoningContent::Text { .. } => None,
                _ => None,
            })
            .collect::<Vec<_>>();
        (!content.is_empty()).then_some(Self { id, content })
    }

    fn to_rig(&self) -> Reasoning {
        let mut reasoning = Reasoning::summaries(Vec::new()).with_id(self.id.clone());
        reasoning.content = self
            .content
            .iter()
            .map(|item| match item {
                ProviderReasoningContent::Summary(text) => ReasoningContent::Summary(text.clone()),
                ProviderReasoningContent::Encrypted(data) => {
                    ReasoningContent::Encrypted(data.clone())
                }
                ProviderReasoningContent::Redacted(data) => {
                    ReasoningContent::Redacted { data: data.clone() }
                }
            })
            .collect();
        reasoning
    }
}

pub fn assistant_message_with_reasoning(
    content: &str,
    reasoning: &[ProviderReasoning],
) -> rig::completion::Message {
    if reasoning.is_empty() {
        return rig::completion::Message::assistant(content.to_string());
    }

    let mut items = reasoning
        .iter()
        .map(|item| AssistantContent::Reasoning(item.to_rig()))
        .collect::<Vec<_>>();
    if !content.is_empty() {
        items.push(AssistantContent::Text(Text::new(content.to_string())));
    }

    rig::completion::Message::Assistant {
        id: None,
        content: OneOrMany::many(items).expect("assistant reasoning message is non-empty"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Compaction {
    pub summary: CompactString,
    pub first_kept_index: usize,
    pub summarized_count: usize,
    pub token_savings: u64,
    pub created_at: CompactString,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionAllowEntry {
    pub tool: CompactString,
    pub pattern: CompactString,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: CompactString,
    pub name: CompactString,
    pub messages: Vec<SessionMessage>,
    pub compactions: Vec<Compaction>,
    pub created_at: CompactString,
    pub updated_at: CompactString,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_cached_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    pub total_cost: f64,
    pub total_estimated_tokens: u64,
    #[serde(default)]
    pub calibrated_tokens: u64,
    #[serde(default)]
    pub calibrated_msg_count: usize,
    #[serde(default)]
    pub input_token_cost: f64,
    #[serde(default)]
    pub output_token_cost: f64,
    pub context_window: u64,
    pub model: CompactString,
    pub provider: CompactString,
    pub working_dir: CompactString,
    #[serde(default)]
    pub permission_allowlist: Vec<PermissionAllowEntry>,
    #[cfg(feature = "multimodal")]
    #[serde(skip)]
    pub pending_media: Vec<crate::extras::multimodal::MediaAttachment>,
    /// Display preference (set from config at startup, not persisted): show the
    /// session cost in the status bar even when it is $0.0000.
    #[serde(skip)]
    pub show_cost_always: bool,
    /// Current git branch of `working_dir`, for the status bar. Refreshed at
    /// runtime, not persisted.
    #[serde(skip)]
    pub git_branch: Option<CompactString>,
    /// Working-tree change counts and upstream sync, for the status bar.
    /// Computed only when the statusline uses a git change/status item. Not persisted.
    #[serde(skip)]
    pub git_status: Option<GitStatus>,
    /// Whether reasoning is currently enabled, for the status bar. Synced from
    /// the event loop, not persisted.
    #[serde(skip)]
    pub reasoning_enabled: bool,
    /// Estimated tokens for the fixed request overhead that never lives in
    /// `messages` — system prompt, tool-use preamble, context files, memory.
    /// Used only before the first real calibration (see
    /// [`effective_context_tokens`](Self::effective_context_tokens)); once the
    /// provider reports real usage, the calibration anchor already includes this
    /// overhead, so it must not be added again. Recomputed at runtime, not
    /// persisted.
    #[serde(skip)]
    pub overhead_tokens: u64,
}

/// Working-tree summary parsed from `git status --porcelain=v2 --branch`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    pub staged: u32,
    pub modified: u32,
    pub deleted: u32,
    pub untracked: u32,
    pub ahead: u32,
    pub behind: u32,
}

impl GitStatus {
    pub fn is_dirty(&self) -> bool {
        self.staged + self.modified + self.deleted + self.untracked > 0
    }
}

impl Session {
    pub fn estimate_tokens(text: &str) -> u64 {
        let mut wide: u64 = 0;
        let mut narrow: u64 = 0;
        for ch in text.chars() {
            if Self::is_wide_char(ch) {
                wide += 1;
            } else {
                narrow += 1;
            }
        }
        // wide * 0.9 + narrow / 4, min 1
        ((wide * 9 / 10) + narrow / 4).max(1)
    }

    fn is_wide_char(ch: char) -> bool {
        matches!(ch as u32,
            0x1100..=0x11FF |   // Hangul Jamo
            0x2E80..=0x9FFF |   // CJK radicals/Kangxi/punctuation/kana/Unified+ExtA
            0xA000..=0xA4CF |   // Yi
            0xAC00..=0xD7A3 |   // Hangul Syllables
            0xF900..=0xFAFF |   // CJK Compatibility Ideographs
            0xFF00..=0xFFEF |   // Halfwidth/Fullwidth Forms
            0x20000..=0x3FFFF   // Supplementary Ideographic Plane (Ext B–F)
        )
    }

    pub fn new(provider: &str, model: &str, context_window: u64) -> Self {
        let now = CompactString::new(chrono::Utc::now().to_rfc3339());
        Session {
            id: CompactString::new(Uuid::new_v4().to_string()),
            name: CompactString::new(""),
            messages: Vec::new(),
            compactions: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            total_input_tokens: 0,
            total_cached_input_tokens: 0,
            total_output_tokens: 0,
            total_cost: 0.0,
            total_estimated_tokens: 0,
            calibrated_tokens: 0,
            calibrated_msg_count: 0,
            input_token_cost: 0.0,
            output_token_cost: 0.0,
            context_window,
            model: CompactString::new(model),
            provider: CompactString::new(provider),
            working_dir: std::env::current_dir()
                .map(|p| CompactString::new(p.to_string_lossy()))
                .unwrap_or_default(),
            permission_allowlist: Vec::new(),
            #[cfg(feature = "multimodal")]
            pending_media: Vec::new(),
            show_cost_always: false,
            git_branch: None,
            git_status: None,
            reasoning_enabled: false,
            overhead_tokens: 0,
        }
    }

    /// Read the current git branch for `dir`, or `None` outside a repo / on a
    /// detached HEAD (then a short commit hash is returned instead). Reads
    /// `.git/HEAD` directly (cheap) rather than spawning git, and follows the
    /// `.git` file pointer used by worktrees and submodules.
    pub fn detect_git_branch(dir: &str) -> Option<CompactString> {
        use std::path::{Path, PathBuf};
        let dir_path = Path::new(dir);
        let dot_git = dir_path.join(".git");
        let gitdir = if dot_git.is_dir() {
            dot_git
        } else if dot_git.is_file() {
            let content = std::fs::read_to_string(&dot_git).ok()?;
            let rel = content.strip_prefix("gitdir:")?.trim();
            let p = PathBuf::from(rel);
            if p.is_absolute() { p } else { dir_path.join(p) }
        } else {
            return None;
        };
        let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
        let head = head.trim();
        if let Some(rest) = head.strip_prefix("ref:") {
            let r = rest.trim();
            Some(CompactString::new(
                r.strip_prefix("refs/heads/").unwrap_or(r),
            ))
        } else if !head.is_empty() {
            // Detached HEAD: show a short commit hash.
            Some(CompactString::new(&head[..head.len().min(8)]))
        } else {
            None
        }
    }

    /// Refresh [`git_branch`](Self::git_branch) from the current `working_dir`.
    pub fn refresh_git_branch(&mut self) {
        self.git_branch = Self::detect_git_branch(&self.working_dir);
    }

    /// Refresh [`git_status`](Self::git_status) by running `git status` in
    /// `working_dir`. Only call this when the statusline actually shows a git
    /// change/status item: it spawns a subprocess (throttled by the caller).
    pub fn refresh_git_status(&mut self) {
        self.git_status = Self::detect_git_status(&self.working_dir);
    }

    fn detect_git_status(dir: &str) -> Option<GitStatus> {
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain=v2", "--branch"])
            .current_dir(dir)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(Self::parse_porcelain(&String::from_utf8_lossy(&out.stdout)))
    }

    /// Parse `git status --porcelain=v2 --branch` output into a [`GitStatus`].
    pub fn parse_porcelain(text: &str) -> GitStatus {
        let mut s = GitStatus::default();
        for line in text.lines() {
            if let Some(ab) = line.strip_prefix("# branch.ab ") {
                // Format: "+<ahead> -<behind>"
                for tok in ab.split_whitespace() {
                    if let Some(n) = tok.strip_prefix('+') {
                        s.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = tok.strip_prefix('-') {
                        s.behind = n.parse().unwrap_or(0);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("1 ").or_else(|| line.strip_prefix("2 ")) {
                // Changed/renamed entry. The XY field is the first token: index
                // status (staged) then worktree status.
                if let Some(xy) = rest.split_whitespace().next() {
                    let mut chars = xy.chars();
                    let x = chars.next().unwrap_or('.');
                    let y = chars.next().unwrap_or('.');
                    if x != '.' {
                        s.staged += 1;
                    }
                    match y {
                        'M' => s.modified += 1,
                        'D' => s.deleted += 1,
                        _ => {}
                    }
                }
            } else if line.starts_with("u ") {
                // Unmerged paths count as a working-tree modification.
                s.modified += 1;
            } else if line.starts_with("? ") {
                s.untracked += 1;
            }
        }
        s
    }

    pub fn add_message(&mut self, role: MessageRole, content: &str) {
        self.add_message_with_reasoning(role, content, Vec::new());
    }

    pub fn add_message_with_reasoning(
        &mut self,
        role: MessageRole,
        content: &str,
        provider_reasoning: Vec<ProviderReasoning>,
    ) {
        self.add_message_with_reasoning_and_usage(role, content, provider_reasoning, None);
    }

    pub fn add_message_with_reasoning_and_usage(
        &mut self,
        role: MessageRole,
        content: &str,
        provider_reasoning: Vec<ProviderReasoning>,
        provider_usage: Option<SessionTokenUsage>,
    ) {
        let tokens = Self::estimate_tokens(content);
        self.messages.push(SessionMessage {
            role,
            content: CompactString::new(content),
            estimated_tokens: tokens,
            provider_reasoning,
            provider_usage,
            tool_call: None,
            tool_result: None,
        });
        self.total_estimated_tokens = self.total_estimated_tokens.saturating_add(tokens);
        self.updated_at = CompactString::new(chrono::Utc::now().to_rfc3339());
    }

    #[allow(dead_code)]
    pub fn add_tool_call(&mut self, name: &str, args: &serde_json::Value) {
        let id = format!("session-tool-{}", self.messages.len());
        self.add_tool_call_structured(name, args, &id, None);
    }

    pub fn add_tool_call_structured(
        &mut self,
        name: &str,
        args: &serde_json::Value,
        id: &str,
        call_id: Option<&str>,
    ) {
        let content = crate::ui::utils::format_tool_call_summary(name, args);
        let tokens = Self::estimate_tokens(&content);
        self.messages.push(SessionMessage {
            role: MessageRole::ToolCall,
            content: CompactString::new(content),
            estimated_tokens: tokens,
            provider_reasoning: Vec::new(),
            provider_usage: None,
            tool_call: Some(SessionToolCall {
                id: CompactString::new(id),
                call_id: call_id.map(CompactString::new),
                name: CompactString::new(name),
                arguments: args.clone(),
            }),
            tool_result: None,
        });
        self.total_estimated_tokens = self.total_estimated_tokens.saturating_add(tokens);
        self.updated_at = CompactString::new(chrono::Utc::now().to_rfc3339());
    }

    #[allow(dead_code)]
    pub fn add_tool_result(&mut self, name: &str, output: &str) -> String {
        let (id, call_id) = self
            .messages
            .iter()
            .rev()
            .find_map(|msg| msg.tool_call.as_ref())
            .map(|call| {
                (
                    call.id.to_string(),
                    call.call_id.as_ref().map(ToString::to_string),
                )
            })
            .unwrap_or_else(|| (format!("session-tool-result-{}", self.messages.len()), None));
        self.add_tool_result_structured(name, output, &id, call_id.as_deref())
    }

    pub fn add_tool_result_structured(
        &mut self,
        name: &str,
        output: &str,
        id: &str,
        call_id: Option<&str>,
    ) -> String {
        self.add_tool_result_structured_with_context(name, output, id, call_id, Vec::new())
    }

    pub fn add_tool_result_structured_with_context(
        &mut self,
        name: &str,
        output: &str,
        id: &str,
        call_id: Option<&str>,
        loaded_context: Vec<String>,
    ) -> String {
        let content = self.tool_result_content(name, output);
        let tokens = Self::estimate_tokens(&content);
        self.messages.push(SessionMessage {
            role: MessageRole::ToolResult,
            content: CompactString::new(&content),
            estimated_tokens: tokens,
            provider_reasoning: Vec::new(),
            provider_usage: None,
            tool_call: None,
            tool_result: Some(SessionToolResult {
                id: CompactString::new(id),
                call_id: call_id.map(CompactString::new),
                name: CompactString::new(name),
                loaded_context: loaded_context.into_iter().map(CompactString::new).collect(),
            }),
        });
        self.total_estimated_tokens = self.total_estimated_tokens.saturating_add(tokens);
        self.updated_at = CompactString::new(chrono::Utc::now().to_rfc3339());
        content
    }

    fn tool_result_content(&self, name: &str, output: &str) -> String {
        let output_chars = output.chars().count();
        if output_chars <= TOOL_RESULT_SAVE_THRESHOLD {
            return format!("{name}:\n{output}");
        }

        match storage::save_tool_output(&self.id, name, output) {
            Ok(path) => format_truncated_tool_result(name, output, output_chars, &path),
            Err(err) => format!(
                "{name}:\n{output}\n\n[failed to save long tool output separately; kept full output in session to avoid data loss: {err}]"
            ),
        }
    }

    pub fn add_subagent_tool_call(&mut self, name: &str, args: &serde_json::Value) {
        self.add_message(
            MessageRole::SubagentToolCall,
            &crate::ui::utils::format_tool_call_summary(name, args),
        );
    }

    pub fn loaded_read_context_paths(&self) -> Vec<PathBuf> {
        self.messages
            .iter()
            .filter_map(|msg| msg.tool_result.as_ref())
            .filter(|result| result.name == "read")
            .flat_map(|result| result.loaded_context.iter())
            .map(|path| PathBuf::from(path.as_str()))
            .collect()
    }

    pub fn title(&self) -> String {
        if !self.name.is_empty() {
            return self.name.to_string();
        }
        self.messages
            .iter()
            .rev()
            .find(|msg| msg.role == MessageRole::User)
            .map(|msg| msg.content.chars().take(80).collect())
            .unwrap_or_else(|| "untitled".to_string())
    }

    pub fn fork_title(&self, message_index: usize) -> String {
        let target = self
            .messages
            .get(message_index)
            .filter(|msg| msg.role == MessageRole::User)
            .map(|msg| msg.content.chars().take(48).collect::<String>())
            .unwrap_or_else(|| self.title());
        format!("Fork before: {target}")
    }

    pub fn fork_before_message(&self, message_index: usize) -> Self {
        let mut fork = self.clone();
        let now = CompactString::new(chrono::Utc::now().to_rfc3339());
        fork.id = CompactString::new(Uuid::new_v4().to_string());
        fork.name = CompactString::new(self.fork_title(message_index));
        fork.created_at = now.clone();
        fork.updated_at = now;
        fork.messages
            .truncate(message_index.min(fork.messages.len()));
        fork.total_estimated_tokens = fork.messages.iter().map(|m| m.estimated_tokens).sum();
        fork.total_input_tokens = 0;
        fork.total_cached_input_tokens = 0;
        fork.total_output_tokens = 0;
        fork.total_cost = 0.0;
        fork.input_token_cost = 0.0;
        fork.output_token_cost = 0.0;
        fork.reset_calibration();
        if fork.messages.is_empty()
            || fork
                .compactions
                .last()
                .is_some_and(|c| c.first_kept_index > fork.messages.len())
        {
            fork.compactions.clear();
        }
        fork
    }

    #[cfg(feature = "multimodal")]
    pub fn drain_media(&mut self) -> Vec<crate::extras::multimodal::MediaAttachment> {
        std::mem::take(&mut self.pending_media)
    }

    /// The true prompt size occupying the context window, normalizing across
    /// providers' differing cache-usage reporting.
    ///
    /// The Anthropic-native route reports `input_tokens` counting *only* the
    /// uncached portion of the prompt; the cache-read and cache-creation tokens
    /// are reported in separate fields even though they still occupy the context
    /// window. So there the real prompt size is the sum of all three. The
    /// OpenAI, Gemini and OpenRouter shapes instead fold the cached subset into
    /// `input_tokens` and report no cache-creation, so `input_tokens` is already
    /// the full prompt size and adding the cache fields would double-count.
    ///
    /// `anthropic_native` must be the *resolved protocol route*, not a literal
    /// provider-name comparison — a custom gateway with `provider_type =
    /// "anthropic"` uses the native route under a different name, while
    /// OpenRouter serving a Claude model does not. Compute it with
    /// [`Config::is_anthropic_native`](crate::config::Config::is_anthropic_native).
    pub fn real_input_tokens(
        anthropic_native: bool,
        input_tokens: u64,
        cached_input_tokens: u64,
        cache_creation_input_tokens: u64,
    ) -> u64 {
        if anthropic_native {
            input_tokens
                .saturating_add(cached_input_tokens)
                .saturating_add(cache_creation_input_tokens)
        } else {
            input_tokens
        }
    }

    pub fn set_calibration(&mut self, input_tokens: u64, output_tokens: u64) {
        if input_tokens == 0 {
            return;
        }
        self.calibrated_tokens = input_tokens.saturating_add(output_tokens);
        self.calibrated_msg_count = self.messages.len();
    }

    pub fn reset_calibration(&mut self) {
        self.calibrated_tokens = 0;
        self.calibrated_msg_count = 0;
    }

    pub fn truncate_to(&mut self, new_len: usize) {
        if new_len >= self.messages.len() {
            return;
        }
        let cal = self.calibrated_msg_count.min(self.messages.len());
        if self.calibrated_tokens > 0 && new_len < cal {
            let removed: u64 = self.messages[new_len..cal]
                .iter()
                .map(|m| m.estimated_tokens)
                .sum();
            self.calibrated_tokens = self.calibrated_tokens.saturating_sub(removed);
            self.calibrated_msg_count = new_len;
        }
        self.messages.truncate(new_len);
        self.total_estimated_tokens = self.estimated_message_tokens();
    }

    pub fn ctx_is_estimated(&self) -> bool {
        self.latest_provider_context_tokens().is_none()
    }

    fn estimated_message_tokens(&self) -> u64 {
        self.messages.iter().map(|m| m.estimated_tokens).sum()
    }

    pub fn latest_provider_context_tokens(&self) -> Option<u64> {
        self.messages
            .iter()
            .rev()
            .filter(|msg| msg.role == MessageRole::Assistant)
            .filter_map(|msg| msg.provider_usage.map(SessionTokenUsage::context_tokens))
            .find(|tokens| *tokens > 0)
    }

    pub fn effective_context_tokens(&self) -> u64 {
        if let Some(tokens) = self.latest_provider_context_tokens() {
            return tokens;
        }
        self.overhead_tokens
            .saturating_add(self.total_estimated_tokens)

    }

    pub fn select_compaction_cut(messages: &[SessionMessage], keep_recent: u64) -> usize {
        let mut accumulated = 0u64;
        let mut cut_idx = 0;
        for (i, msg) in messages.iter().enumerate().rev() {
            if accumulated >= keep_recent {
                cut_idx = i + 1;
                break;
            }
            accumulated = accumulated.saturating_add(msg.estimated_tokens);
        }
        cut_idx
    }

    pub fn needs_compaction(&self, reserve_tokens: u64) -> bool {
        if self.context_window == 0 {
            return false;
        }
        self.effective_context_tokens() > self.context_window.saturating_sub(reserve_tokens)
    }

    pub fn update_context_window(&mut self, cw: u64) {
        self.context_window = cw;
    }

    pub fn compacted_context(&self) -> (Option<&str>, usize) {
        let c = match self.compactions.last() {
            Some(c) => c,
            None => return (None, 0),
        };
        // Locate the summary System message at runtime rather than trusting
        // a stored index, which drifts if messages are inserted before it.
        for (i, msg) in self.messages.iter().enumerate() {
            if msg.role == MessageRole::System && msg.content.as_str() == c.summary.as_str() {
                return (Some(c.summary.as_str()), i + 1);
            }
        }
        (None, 0)
    }

    pub fn compress(&mut self, summary: String, first_kept_index: usize, token_savings: u64) {
        let summarized_count = first_kept_index;
        let summary_tokens = Self::estimate_tokens(&summary);

        // Insert a System message with the summary at the boundary
        let summary_msg = SessionMessage {
            role: MessageRole::System,
            content: CompactString::from(summary.clone()),
            estimated_tokens: summary_tokens,
            provider_reasoning: Vec::new(),
            provider_usage: None,
            tool_call: None,
            tool_result: None,
        };

        // Remove summarized messages and insert summary
        self.messages.drain(..first_kept_index);
        self.messages.insert(0, summary_msg);
        for msg in &mut self.messages {
            msg.provider_usage = None;
        }
        self.total_estimated_tokens = self.estimated_message_tokens();

        // Recompute total from remaining messages so the count is always
        // consistent — no underflow risk when token_savings is stale.
        self.total_estimated_tokens = self.messages.iter().map(|m| m.estimated_tokens).sum();

        self.compactions.push(Compaction {
            summary: CompactString::from(summary),
            first_kept_index: 1, // The summary is at index 0
            summarized_count,
            token_savings,
            created_at: CompactString::new(chrono::Utc::now().to_rfc3339()),
        });

        // Compaction reindexes messages, so the calibration anchor no longer
        // lines up. Drop it; the next completed turn re-anchors.
        self.reset_calibration();
        self.updated_at = CompactString::new(chrono::Utc::now().to_rfc3339());
    }
}

#[cfg(test)]
mod tests {
    use super::Session;

    #[test]
    fn tool_result_loaded_context_round_trips() {
        let mut session = Session::new("openai", "gpt-5.1", 128000);
        session.add_tool_result_structured_with_context(
            "read",
            "output",
            "call_1",
            None,
            vec!["/repo/src/AGENTS.md".to_string()],
        );

        let json = serde_json::to_string(&session).unwrap();
        let loaded: Session = serde_json::from_str(&json).unwrap();

        assert_eq!(
            loaded.loaded_read_context_paths(),
            vec![std::path::PathBuf::from("/repo/src/AGENTS.md")]
        );
    }

    #[test]
    fn compaction_drops_summarized_loaded_context_metadata() {
        let mut session = Session::new("openai", "gpt-5.1", 128000);
        session.add_tool_result_structured_with_context(
            "read",
            "old",
            "call_1",
            None,
            vec!["/repo/old/AGENTS.md".to_string()],
        );
        session.add_tool_result_structured_with_context(
            "read",
            "kept",
            "call_2",
            None,
            vec!["/repo/kept/AGENTS.md".to_string()],
        );

        session.compress("summary".to_string(), 1, 10);

        assert_eq!(
            session.loaded_read_context_paths(),
            vec![std::path::PathBuf::from("/repo/kept/AGENTS.md")]
        );
    }
}

fn format_truncated_tool_result(
    name: &str,
    output: &str,
    output_chars: usize,
    path: &Path,
) -> String {
    let head: String = output.chars().take(TOOL_RESULT_HEAD_CHARS).collect();
    let tail_start = output_chars.saturating_sub(TOOL_RESULT_TAIL_CHARS);
    let tail: String = output.chars().skip(tail_start).collect();
    let omitted = output_chars.saturating_sub(TOOL_RESULT_HEAD_CHARS + TOOL_RESULT_TAIL_CHARS);

    format!(
        "{name}:\n{head}\n\n[tool output truncated: {output_chars} characters; {omitted} omitted]\n[full output saved to: {}; use the read tool on this path to inspect the complete output]\n\n{tail}",
        path.display()
    )
}
