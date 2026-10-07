use std::collections::{HashMap, HashSet};
use std::time::Duration;

#[cfg(feature = "multimodal")]
use base64::Engine;
use compact_str::CompactString;
use futures::StreamExt;
use rig::agent::{Agent, MultiTurnStreamItem, StreamingResult};
#[cfg(feature = "subagents")]
use rig::agent::{AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext};
use rig::completion::message::{
    AssistantContent, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
};
#[cfg(feature = "multimodal")]
use rig::completion::message::{
    AudioMediaType, Document, DocumentMediaType, DocumentSourceKind, ImageMediaType, MimeType,
};
use rig::completion::{Message, PromptError};
use rig::streaming::{Item, StreamEvent};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, sleep_until};

use crate::event::{AgentEvent, BtwEvent, ProviderCall, TokenUsage};
use crate::session::{
    MessageRole, ProviderReasoning, Session, SessionMessage, assistant_message_with_reasoning,
};

#[derive(Clone, Default)]
struct ProviderTurnHook(std::sync::Arc<std::sync::Mutex<Option<rig::message::AssistantMessage>>>);

impl ProviderTurnHook {
    fn take(&self) -> Option<rig::message::AssistantMessage> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}

impl rig::agent::AgentHook for ProviderTurnHook {
    async fn on_outcome(
        &self,
        _ctx: &rig::agent::HookContext,
        event: rig::agent::OutcomeEvent<'_>,
    ) -> rig::agent::OutcomeAction {
        if let Ok(rig::effect::Outcome::Completion(response)) = event.outcome
            && let Some(Message::Assistant(message)) = response.message()
        {
            *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(message);
        }
        rig::agent::OutcomeAction::Proceed
    }
}

pub struct AgentRunner {
    pub event_rx: mpsc::Receiver<AgentEvent>,
    /// Cancels the underlying agent task. Without this a superseded or
    /// interrupted run keeps driving its stream — and therefore keeps executing
    /// tools (edit/write/bash) — invisibly. Aborting stops it for real.
    pub abort_handle: tokio::task::AbortHandle,
}

pub struct PrintRunResult {
    pub response: String,
    pub reasoning: Vec<ProviderReasoning>,
    pub usage: TokenUsage,
    pub context_usage: TokenUsage,
    pub provider_calls: Vec<ProviderCall>,
}

/// Handle to an in-flight `/btw` side-question task. The `abort_handle` lets the
/// UI cancel the side question (e.g. on Ctrl-C) without touching the main agent.
pub struct BtwRunner {
    pub abort_handle: tokio::task::AbortHandle,
}

fn done_usages(
    usage_total: TokenUsage,
    latest_usage: Option<TokenUsage>,
    final_usage: TokenUsage,
) -> (TokenUsage, TokenUsage) {
    let billing_usage = if usage_total == TokenUsage::default() {
        final_usage
    } else {
        usage_total
    };
    let context_usage = latest_usage.unwrap_or(billing_usage);
    (billing_usage, context_usage)
}

fn streamed_reasoning_text(content: &Item<StreamEvent>) -> Option<CompactString> {
    match content {
        Item::Event(StreamEvent::Reasoning { text, .. }) if !text.is_empty() => {
            Some(CompactString::new(text))
        }
        _ => None,
    }
}

fn streamed_provider_reasoning(content: &Item<StreamEvent>) -> Option<ProviderReasoning> {
    match content {
        Item::Event(StreamEvent::End {
            content: AssistantContent::Reasoning(reasoning),
            ..
        }) => ProviderReasoning::from_rig(reasoning),
        _ => None,
    }
}

fn streamed_assistant_content_has_output(content: &Item<StreamEvent>) -> bool {
    match content {
        Item::Event(StreamEvent::Text { text, .. }) => !text.is_empty(),
        Item::Event(StreamEvent::End {
            content: AssistantContent::ToolCall(_),
            ..
        }) => true,
        _ => {
            streamed_reasoning_text(content).is_some()
                || streamed_provider_reasoning(content).is_some()
        }
    }
}

const PROVIDER_RETRY_INITIAL_DELAY_MS: u64 = 2_000;
const PROVIDER_RETRY_MAX_DELAY_MS: u64 = 30_000;
const PROVIDER_RETRY_CONTINUE_PROMPT: &str = "Go";

pub(crate) fn retry_delay_ms(attempt: usize, error: &PromptError) -> Option<u64> {
    let retryable = match error {
        PromptError::Provider(error) => {
            error.is_retryable() || is_retryable_provider_error(&error.to_string())
        }
        PromptError::Report(report) => {
            report.retryable || is_retryable_provider_error(&report.to_string())
        }
        _ => false,
    };
    retryable.then(|| {
        PROVIDER_RETRY_INITIAL_DELAY_MS
            .saturating_mul(1_u64 << attempt.min(4))
            .min(PROVIDER_RETRY_MAX_DELAY_MS)
    })
}

fn is_retryable_http_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..=599).contains(&status)
}

fn is_retryable_provider_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if is_non_retryable_provider_error(&lower) {
        return false;
    }
    if let Some((code, _)) = lower.split_once(':')
        && [
            "server_error",
            "internal_error",
            "api_error",
            "overloaded_error",
            "rate_limit_error",
            "rate_limit_exceeded",
            "provider_overloaded",
            "provider_unavailable",
            "resource_exhausted",
            "unavailable",
            "internal",
            "server",
            "timeout",
            "unmapped",
        ]
        .contains(&code.trim())
    {
        return true;
    }
    [
        "timeout",
        "timed out",
        "deadline exceeded",
        "rate limit",
        "rate_limit",
        "too many requests",
        "too_many_requests",
        "overloaded",
        "overload",
        "exhausted",
        "temporarily unavailable",
        "provider_unavailable",
        "server_error",
        "internal_error",
        "internal server error",
        "service unavailable",
        "gateway timeout",
        "bad gateway",
        "connection lost",
        "connection reset",
        "connection closed",
        "error sending request",
        "temporary failure",
        "error decoding response body",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || lower
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| part.len() == 3)
            .filter_map(|part| part.parse::<u16>().ok())
            .any(is_retryable_http_status)
}

fn is_non_retryable_provider_error(lower: &str) -> bool {
    [
        "context_length_exceeded",
        "context length",
        "context window",
        "prompt is too long",
        "input is too long",
        "maximum context",
        "max context",
        "too large for model",
        "no tool output found",
        "invalid_request_error",
        "invalid api key",
        "unauthorized",
        "forbidden",
        "permission denied",
        "insufficient_quota",
        "billing",
        "usage limit",
        "freeusagelimiterror",
        "gousagelimiterror",
        "model not found",
        "unsupported model",
        "content policy",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(crate) async fn sleep_for_retry(delay_ms: u64) {
    sleep(Duration::from_millis(delay_ms)).await;
}

fn push_tool_call(tool_interactions: &mut Vec<Message>, tool_call: ToolCall) {
    if let Some(Message::Assistant(rig::message::AssistantMessage { content, .. })) =
        tool_interactions.last_mut()
    {
        content.push(AssistantContent::ToolCall(tool_call));
    } else {
        tool_interactions.push(Message::Assistant(rig::message::AssistantMessage::new(
            vec![AssistantContent::ToolCall(tool_call)],
        )));
    }
}

fn preserve_retry_output(
    history: &mut Vec<Message>,
    prompt: &str,
    tool_interactions: &mut Vec<Message>,
    partial_text: &str,
) {
    history.push(Message::user(prompt));
    history.append(tool_interactions);
    if !partial_text.is_empty() {
        history.push(Message::assistant(partial_text));
    }
}

fn prepare_retry_continuation(
    history: &mut Vec<Message>,
    prompt: &mut String,
    tool_interactions: &mut Vec<Message>,
    partial_text: &str,
) {
    preserve_retry_output(history, prompt, tool_interactions, partial_text);
    *prompt = PROVIDER_RETRY_CONTINUE_PROMPT.to_string();
}

/// Spawn an isolated, single-turn, tool-less side-question run. The full result
/// is delivered as a single [`BtwEvent::Done`] (or [`BtwEvent::Error`]) tagged
/// with `id`. Unlike [`spawn_agent`], it never registers a subagent event sink
/// and never mutates the session.
pub fn spawn_btw(
    agent: Agent,
    prompt: String,
    history: Vec<Message>,
    event_tx: mpsc::Sender<BtwEvent>,
    id: u32,
) -> BtwRunner {
    let join = tokio::spawn(async move {
        let mut stream = agent.prompt(prompt).history(history).stream();
        let mut acc = String::new();

        while let Some(item) = stream.next().await {
            match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                    text,
                    ..
                }))) => acc.push_str(&text),
                Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                    let response_text = res.output();
                    let usage = res.usage();
                    let response = if response_text.is_empty() {
                        CompactString::from(acc.as_str())
                    } else {
                        CompactString::from(response_text)
                    };
                    let _ = event_tx
                        .send(BtwEvent::Done {
                            id,
                            response,
                            usage: usage.into(),
                        })
                        .await;
                    return;
                }
                Err(e) => {
                    let _ = event_tx
                        .send(BtwEvent::Error {
                            id,
                            message: CompactString::new(e.to_string()),
                        })
                        .await;
                    return;
                }
                _ => {}
            }
        }

        let _ = event_tx
            .send(BtwEvent::Error {
                id,
                message: CompactString::new("side question ended without a response"),
            })
            .await;
    });

    BtwRunner {
        abort_handle: join.abort_handle(),
    }
}

pub fn convert_history(session: &Session) -> Vec<Message> {
    crate::agent::tools::set_active_session_id(Some(session.id.to_string()));
    crate::agent::tools::reset_read_context_loaded(session.loaded_read_context_paths());
    convert_history_inner(session)
}

fn convert_history_inner(session: &Session) -> Vec<Message> {
    let (summary, first_kept) = session.compacted_context();
    let remaining = session.messages.len().saturating_sub(first_kept);
    let extra = if summary.is_some() { 1 } else { 0 };
    let mut messages = Vec::with_capacity(remaining + extra);

    // The compaction summary is emitted as an Assistant message rather
    // than a System message: the agent already has a System preamble
    // (SYSTEM_PROMPT + mode prompt + context files), and some model chat
    // templates (notably Qwen 3.x) refuse any System message past
    // position 0. Assistant role also produces clean User↔Assistant
    // alternation when the next user prompt arrives, which reads as
    // "the agent recaps what it did, then the user continues" — a
    // natural resumed-conversation shape. The "[Recap of my prior work
    // in this conversation]" prefix labels the message as a self-recap
    // so the agent doesn't treat it as a fresh continuation of its own
    // voice.
    if let Some(summary) = summary {
        messages.push(Message::assistant(format!(
            "[Recap of my prior work in this conversation]\n{}",
            summary
        )));
    }

    let replayed_tool_result_ids = replayed_tool_result_ids(&session.messages[first_kept..]);
    let requires_call_id = matches!(
        session.provider.as_str(),
        "openai" | "openai-codex" | "codex"
    );
    let mut replayed_tool_call_ids = HashMap::new();

    for msg in &session.messages[first_kept..] {
        match msg.role {
            MessageRole::User => {
                #[cfg(feature = "multimodal")]
                for attachment in &msg.attachments {
                    match crate::extras::multimodal::load_persisted_attachment(
                        &session.id,
                        attachment,
                    ) {
                        Ok(media) => messages.extend(media_to_messages(&[media])),
                        Err(error) => tracing::warn!(
                            "failed to load session attachment {}: {error}",
                            attachment.filename
                        ),
                    }
                }
                messages.push(Message::user(msg.content.to_string()));
            }
            MessageRole::Assistant => {
                let mut message =
                    assistant_message_with_reasoning(&msg.content, &msg.provider_reasoning);
                if let Message::Assistant(turn) = &mut message
                    && turn.origin.is_none()
                    && session.provider == "opencode-go"
                    && session.model.starts_with("deepseek-")
                {
                    turn.origin = Some(rig::message::Origin::new(
                        "openai.chat",
                        "openai",
                        session.model.to_string(),
                    ));
                }
                messages.push(message);
            }
            // Convert non-user transcript records to Assistant for the
            // same reason as the summary above: the templates that reject
            // mid-stream System/tool roles tolerate Assistant, and code-symmetry with
            // the summary push keeps the resumed-conversation shape
            // consistent.
            MessageRole::System => messages.push(Message::assistant(msg.content.to_string())),
            MessageRole::ToolCall => {
                if let Some(call) = msg.tool_call.as_ref()
                    && replayed_tool_result_ids.contains(call.id.as_str())
                    && let Some((message, replayed_id, call_id)) =
                        tool_call_message(msg, requires_call_id)
                {
                    replayed_tool_call_ids.insert(call.id.to_string(), (replayed_id, call_id));
                    if let Message::Assistant(rig::message::AssistantMessage {
                        content: tool_content,
                        ..
                    }) = &message
                        && let Some(Message::Assistant(rig::message::AssistantMessage {
                            content,
                            ..
                        })) = messages.last_mut()
                    {
                        for item in tool_content {
                            let native = session.messages.iter().filter_map(|msg| msg.provider_reasoning.iter().find_map(ProviderReasoning::native_message)).flat_map(|message| message.content).find(|candidate| matches!((candidate, item), (AssistantContent::ToolCall(a), AssistantContent::ToolCall(b)) if a.id == b.id));
                            content.push(native.unwrap_or_else(|| item.clone()));
                        }
                    } else {
                        messages.push(message);
                    }
                }
            }
            MessageRole::ToolResult => {
                if let Some(result) = msg.tool_result.as_ref()
                    && let Some((replayed_id, call_id)) =
                        replayed_tool_call_ids.remove(result.id.as_str())
                    && let Some(replayed) =
                        tool_result_messages(msg, &session.id, &replayed_id, call_id.as_deref())
                {
                    messages.extend(replayed);
                }
            }
            MessageRole::SubagentToolCall if replayed_tool_call_ids.is_empty() => messages.push(
                Message::assistant(format!("[SubagentToolCall]: {}", msg.content)),
            ),
            MessageRole::SubagentToolCall => {}
        }
    }

    messages
}

fn replayed_tool_result_ids(messages: &[SessionMessage]) -> HashSet<&str> {
    messages
        .iter()
        .filter_map(|msg| msg.tool_result.as_ref())
        .map(|result| result.id.as_str())
        .collect()
}

fn tool_call_message(
    msg: &SessionMessage,
    requires_call_id: bool,
) -> Option<(Message, String, Option<String>)> {
    let call = msg.tool_call.as_ref()?;
    let id = call
        .call_id
        .as_deref()
        .filter(|id| !id.starts_with("fc_"))
        .unwrap_or(call.id.as_str())
        .to_string();
    let name = rig::message::ToolName::new(call.name.to_string()).ok()?;
    let tool_call = ToolCall::new(
        rig::message::CallId::from_wire(id.clone()),
        ToolFunction::new(name, call.arguments.clone()),
    );
    let _ = requires_call_id;
    Some((
        Message::Assistant(rig::message::AssistantMessage::new(vec![
            AssistantContent::ToolCall(tool_call),
        ])),
        id.clone(),
        Some(id),
    ))
}

fn tool_result_messages(
    msg: &SessionMessage,
    session_id: &str,
    id: &str,
    call_id: Option<&str>,
) -> Option<Vec<Message>> {
    let result = msg.tool_result.as_ref()?;
    let mut output = tool_result_output(msg);
    let mut messages = Vec::new();
    #[cfg(feature = "multimodal")]
    for attachment in &result.attachments {
        match crate::extras::multimodal::load_persisted_attachment(session_id, attachment) {
            Ok(media) => messages.extend(media_to_messages(&[media])),
            Err(error) => output.push_str(&format!(
                "\n[failed to load image attachment {}: {error}]",
                attachment.filename
            )),
        }
    }
    messages.insert(
        0,
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: rig::message::CallId::from_wire(call_id.unwrap_or(id)),
                name: rig::message::ToolName::new(result.name.to_string()).ok()?,
                is_error: false,
                content: vec![ToolResultContent::Text(Text::new(output))],
            })],
        },
    );
    Some(messages)
}

#[cfg(feature = "multimodal")]
fn tool_result_images(content: &[ToolResultContent]) -> Vec<crate::event::ToolResultImage> {
    use base64::Engine;
    use base64::prelude::BASE64_STANDARD;

    content
        .iter()
        .filter_map(|item| {
            let ToolResultContent::Image(image) = item else {
                return None;
            };
            let DocumentSourceKind::Base64(data) = &image.data else {
                return None;
            };
            let mime = image.media_type.as_ref()?.to_mime_type();
            Some(crate::event::ToolResultImage {
                data: BASE64_STANDARD.decode(data).ok()?,
                mime: CompactString::new(mime),
            })
        })
        .collect()
}

fn tool_result_output(msg: &SessionMessage) -> String {
    let Some(result) = msg.tool_result.as_ref() else {
        return msg.content.to_string();
    };
    let prefix = format!("{}:\n", result.name);
    msg.content
        .strip_prefix(&prefix)
        .unwrap_or(msg.content.as_str())
        .to_string()
}

#[cfg(feature = "multimodal")]
pub fn media_to_messages(media: &[crate::extras::multimodal::MediaAttachment]) -> Vec<Message> {
    use base64::Engine;
    use base64::prelude::BASE64_STANDARD;
    use rig::completion::message::UserContent;

    media
        .iter()
        .map(|m| match m {
            crate::extras::multimodal::MediaAttachment::Image { data, mime, .. } => Message::User {
                content: vec![UserContent::image_base64(
                    BASE64_STANDARD.encode(data),
                    Some(image_media_type(mime)),
                    None,
                )],
            },
            crate::extras::multimodal::MediaAttachment::Audio { data, mime, .. } => Message::User {
                content: vec![UserContent::Audio(rig::message::Audio {
                    data: DocumentSourceKind::Base64(BASE64_STANDARD.encode(data)),
                    media_type: Some(audio_media_type(mime)),
                })],
            },
            crate::extras::multimodal::MediaAttachment::Document { data, mime, .. } => {
                Message::User {
                    content: vec![UserContent::Document(Document {
                        data: DocumentSourceKind::Base64(BASE64_STANDARD.encode(data)),
                        media_type: Some(document_media_type(mime)),
                        additional_params: None,
                    })],
                }
            }
        })
        .collect()
}

#[cfg(feature = "multimodal")]
pub(crate) fn image_media_type(mime: &str) -> ImageMediaType {
    match mime {
        "image/png" => ImageMediaType::PNG,
        "image/jpeg" => ImageMediaType::JPEG,
        "image/gif" => ImageMediaType::GIF,
        "image/webp" => ImageMediaType::WEBP,
        _ => unreachable!("unknown image mime type: {mime}"),
    }
}

#[cfg(feature = "multimodal")]
fn audio_media_type(mime: &str) -> AudioMediaType {
    match mime {
        "audio/mpeg" => AudioMediaType::MP3,
        "audio/wav" => AudioMediaType::WAV,
        "audio/ogg" => AudioMediaType::OGG,
        "audio/flac" => AudioMediaType::FLAC,
        "audio/mp4" => AudioMediaType::M4A,
        "audio/aac" => AudioMediaType::AAC,
        _ => unreachable!("unknown audio mime type: {mime}"),
    }
}

#[cfg(feature = "multimodal")]
fn document_media_type(mime: &str) -> DocumentMediaType {
    match mime {
        "application/pdf" => DocumentMediaType::PDF,
        _ => unreachable!("unknown document mime type: {mime}"),
    }
}

async fn continue_prompt_injector(
    agent: &Agent,
    retry_prompt: &str,
    retry_history: &[Message],
    tool_interactions: &[Message],
) -> StreamingResult {
    let mut new_history = retry_history.to_vec();
    new_history.extend_from_slice(tool_interactions);
    new_history.push(Message::user(retry_prompt.to_string()));
    new_history.push(Message::assistant(String::new()));
    agent
        .prompt("Please continue.")
        .history(new_history)
        .stream()
}

/// Builds the forked context for a `/btw` side question: the committed
/// conversation history, plus — when the main agent is mid-task — a synthesized
/// note describing the in-flight turn so the side question can see what the
/// agent is doing right now. The returned messages are a by-value snapshot; the
/// session is never mutated, so there is nothing to roll back afterwards.
pub fn build_btw_snapshot(
    session: &Session,
    turn_trace: &[CompactString],
    main_running: bool,
) -> Vec<Message> {
    let mut snapshot = convert_history_inner(session);
    if main_running && !turn_trace.is_empty() {
        snapshot.push(Message::user(format!(
            "(Context only — the main assistant is working in parallel right now. \
	     Its progress so far this turn:\n{}\nThe last step may still be running. Use this \
	     only if the user's question is about what the main assistant is doing.)",
            turn_trace.join("\n")
        )));
    }
    snapshot
}

pub fn spawn_agent(agent: Agent, prompt: String, history: Vec<Message>) -> AgentRunner {
    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(32);

    #[cfg(feature = "subagents")]
    crate::extras::subagents::set_subagent_event_tx(event_tx.clone());

    let join = tokio::spawn(async move {
        let mut retry_prompt = prompt.clone();
        let mut retry_history: Vec<Message> = history.clone();
        let mut tool_interactions: Vec<Message> = Vec::new();
        let mut last_tool_name: Option<String> = None;
        let mut tool_names: HashMap<rig::message::CallId, String> = HashMap::new();
        let mut tool_starts: HashMap<rig::message::CallId, Instant> = HashMap::new();
        let mut usage_total = TokenUsage::default();
        let mut latest_usage: Option<TokenUsage> = None;
        let mut response_reasoning: Vec<ProviderReasoning> = Vec::new();
        let mut retry_attempts = 0usize;
        let mut stream_had_output = false;
        let mut partial_text = String::new();

        let mut provider_call_started = Instant::now();
        let provider_turn = ProviderTurnHook::default();
        let mut stream = agent
            .prompt(prompt)
            .history(history)
            .add_hook(provider_turn.clone())
            .stream();

        loop {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(MultiTurnStreamItem::StreamAssistantItem(content)) => {
                        stream_had_output |= streamed_assistant_content_has_output(&content);
                        if let Some(reasoning) = streamed_provider_reasoning(&content) {
                            tool_interactions.push(assistant_message_with_reasoning(
                                "",
                                std::slice::from_ref(&reasoning),
                            ));
                            response_reasoning.push(reasoning);
                        }
                        if let Some(reasoning) = streamed_reasoning_text(&content) {
                            let _ = event_tx.send(AgentEvent::Reasoning(reasoning)).await;
                            continue;
                        }

                        match content {
                            Item::Event(StreamEvent::Text { text, .. }) => {
                                partial_text.push_str(&text);
                                let _ = event_tx
                                    .send(AgentEvent::Token(CompactString::from(text)))
                                    .await;
                            }
                            _ => {}
                        }
                    }
                    Ok(MultiTurnStreamItem::ToolCall { mut tool_call }) => {
                        let reasoning = provider_turn.take().map(|message| {
                            if let Some(AssistantContent::ToolCall(call)) = message.content.iter().find(|content| matches!(content, AssistantContent::ToolCall(call) if call.id == tool_call.id)) { tool_call = call.clone(); }
                            vec![ProviderReasoning::from_message(&message)]
                        }).unwrap_or_else(|| std::mem::take(&mut response_reasoning));
                        response_reasoning.clear();
                        last_tool_name = Some(tool_call.function.name.to_string());
                        tool_names
                            .insert(tool_call.id.clone(), tool_call.function.name.to_string());
                        tool_starts.insert(tool_call.id.clone(), Instant::now());
                        push_tool_call(&mut tool_interactions, tool_call.clone());
                        let _ = event_tx
                            .send(AgentEvent::ToolCall {
                                id: CompactString::new(tool_call.id.to_string()),
                                call_id: tool_call
                                    .id
                                    .provider()
                                    .map(|id| CompactString::new(id.as_str())),
                                name: CompactString::new(tool_call.function.name.as_str()),
                                args: serde_json::Value::Object(tool_call.function.arguments),
                                reasoning,
                            })
                            .await;
                    }
                    Ok(MultiTurnStreamItem::ToolResult { tool_result }) => {
                        stream_had_output = true;
                        let mut output = String::new();
                        for c in tool_result.content.iter() {
                            if let ToolResultContent::Text(t) = c {
                                if !output.is_empty() {
                                    output.push('\n');
                                }
                                output.push_str(&t.text);
                            }
                        }
                        #[cfg(feature = "multimodal")]
                        let images = tool_result_images(&tool_result.content);
                        #[cfg(not(feature = "multimodal"))]
                        let images = Vec::new();
                        let name = tool_names
                            .remove(&tool_result.call)
                            .or_else(|| last_tool_name.take())
                            .unwrap_or_default();
                        let loaded_context = if name == "read" {
                            crate::agent::tools::take_read_context_metadata(&output)
                        } else {
                            Vec::new()
                        };
                        let duration_ms = tool_starts
                            .remove(&tool_result.call)
                            .map(|start| start.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
                            .unwrap_or(0);
                        let display_artifact = if name == "edit" {
                            crate::agent::tools::edit::take_last_edit_display_artifact()
                        } else {
                            None
                        };
                        let _ = event_tx
                            .send(AgentEvent::ToolResult {
                                id: CompactString::new(tool_result.call.to_string()),
                                call_id: tool_result
                                    .call
                                    .provider()
                                    .map(|id| CompactString::new(id.as_str())),
                                name: CompactString::new(name),
                                output: CompactString::from(output.clone()),
                                images: images.clone(),
                                loaded_context,
                                duration_ms,
                                display_artifact,
                            })
                            .await;
                        #[cfg(feature = "multimodal")]
                        if !images.is_empty() {
                            tool_interactions.push(Message::User {
                                content: vec![UserContent::ToolResult(ToolResult {
                                    call: tool_result.call,
                                    name: tool_result.name,
                                    is_error: tool_result.is_error,
                                    content: vec![ToolResultContent::Text(Text::new(output))],
                                })],
                            });
                            for image in images {
                                tool_interactions.push(Message::User {
                                    content: vec![UserContent::image_base64(
                                        base64::prelude::BASE64_STANDARD.encode(image.data),
                                        Some(image_media_type(&image.mime)),
                                        None,
                                    )],
                                });
                            }
                            break;
                        }
                        tool_interactions.push(tool_result.clone().into());
                        provider_call_started = Instant::now();
                    }
                    Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                        let response_text = res.output();
                        let final_usage = res.usage().into();

                        if !response_text.is_empty() {
                            let (usage, context_usage) =
                                done_usages(usage_total, latest_usage, final_usage);
                            let reasoning = provider_turn
                                .take()
                                .map(|message| vec![ProviderReasoning::from_message(&message)])
                                .unwrap_or_else(|| std::mem::take(&mut response_reasoning));
                            let _ = event_tx
                                .send(AgentEvent::Done {
                                    response: CompactString::from(response_text),
                                    usage,
                                    context_usage,
                                    reasoning,
                                })
                                .await;
                            return;
                        }
                        break;
                    }
                    Ok(MultiTurnStreamItem::CompletionCall(call)) => {
                        let duration_ms = provider_call_started
                            .elapsed()
                            .as_millis()
                            .try_into()
                            .unwrap_or(u64::MAX);
                        let usage = TokenUsage::from(call.usage);
                        usage_total += usage;
                        latest_usage = Some(usage);
                        let _ = event_tx
                            .send(AgentEvent::CompletionCall {
                                call_index: call.call_index,
                                usage,
                                duration_ms,
                            })
                            .await;
                        provider_call_started = Instant::now();
                    }
                    Ok(_) => {}
                    Err(e) => {
                        let message = e.to_string();
                        if let Some(delay_ms) = retry_delay_ms(retry_attempts, &e) {
                            retry_attempts = retry_attempts.saturating_add(1);
                            let continuing = stream_had_output;
                            if continuing {
                                prepare_retry_continuation(
                                    &mut retry_history,
                                    &mut retry_prompt,
                                    &mut tool_interactions,
                                    &partial_text,
                                );
                            }
                            partial_text.clear();
                            stream_had_output = false;
                            let _ = event_tx
                                .send(AgentEvent::Retry {
                                    attempt: retry_attempts.try_into().unwrap_or(u32::MAX),
                                    delay_ms,
                                    message: CompactString::new(message),
                                    continuing,
                                })
                                .await;
                            sleep_for_retry(delay_ms).await;
                            provider_call_started = Instant::now();
                            stream = agent
                                .prompt(retry_prompt.clone())
                                .history(retry_history.clone())
                                .add_hook(provider_turn.clone())
                                .stream();
                            continue;
                        }
                        let reasoning = std::mem::take(&mut response_reasoning);
                        let _ = event_tx
                            .send(AgentEvent::Error {
                                message: CompactString::new(message),
                                reasoning,
                            })
                            .await;
                        return;
                    }
                }
            }

            retry_attempts = 0;
            stream_had_output = false;
            partial_text.clear();
            provider_call_started = Instant::now();
            stream =
                continue_prompt_injector(&agent, &retry_prompt, &retry_history, &tool_interactions)
                    .await;
        }
    });

    AgentRunner {
        event_rx,
        abort_handle: join.abort_handle(),
    }
}

pub async fn run_print(
    agent: &Agent,
    prompt: &str,
    max_turns: usize,
    pure_stdout: bool,
) -> anyhow::Result<PrintRunResult> {
    let mut provider_call_started = Instant::now();
    let mut stream = agent
        .prompt(prompt.to_string())
        .history(Vec::<Message>::new())
        .max_turns(max_turns)
        .stream();

    let mut full_response = String::new();
    let mut response_reasoning = Vec::new();
    let mut usage_total = TokenUsage::default();
    let mut latest_usage: Option<TokenUsage> = None;
    let mut provider_calls = Vec::new();
    let mut last_tool_name: Option<String> = None;

    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                text,
                ..
            }))) => {
                full_response.push_str(&text);
                print!("{}", text);
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::End {
                content: AssistantContent::Reasoning(r),
                ..
            }))) => {
                if let Some(reasoning) = ProviderReasoning::from_rig(&r) {
                    response_reasoning.push(reasoning);
                }
                let _ = std::io::Write::flush(&mut std::io::stderr());
            }
            Ok(MultiTurnStreamItem::ToolCall { tool_call }) if pure_stdout => {
                let name = &tool_call.function.name;
                last_tool_name = Some(name.to_string());
                let summary = format_tool_args_summary(&serde_json::Value::Object(
                    tool_call.function.arguments.clone(),
                ));
                println!("\n◈ {} {}", name, summary);
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            Ok(MultiTurnStreamItem::ToolResult { tool_result }) => {
                if pure_stdout {
                    let name = last_tool_name.take().unwrap_or_default();
                    let mut output = String::new();
                    for c in tool_result.content.iter() {
                        if let ToolResultContent::Text(t) = c {
                            if !output.is_empty() {
                                output.push('\n');
                            }
                            output.push_str(&t.text);
                        }
                    }
                    if !output.is_empty() {
                        println!("◈ {} result:", name);
                        let lines: Vec<&str> = output.lines().collect();
                        if lines.len() > 40 {
                            let truncated: Vec<&str> = lines.iter().take(40).copied().collect();
                            println!("{}", truncated.join("\n"));
                            println!("(truncated {} more lines)", lines.len().saturating_sub(40));
                        } else {
                            println!("{}", output);
                        }
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                }
                provider_call_started = Instant::now();
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Reasoning {
                text,
                ..
            }))) => {
                eprint!("{text}");
                let _ = std::io::Write::flush(&mut std::io::stderr());
            }
            Ok(MultiTurnStreamItem::CompletionCall(call)) => {
                let duration_ms = provider_call_started
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX);
                let usage = TokenUsage::from(call.usage);
                usage_total += usage;
                latest_usage = Some(usage);
                provider_calls.push(ProviderCall {
                    call_index: call.call_index,
                    usage,
                    duration_ms,
                });
                provider_call_started = Instant::now();
            }
            Ok(MultiTurnStreamItem::FinalResponse(response)) => {
                if let Some(Message::Assistant(message)) = response.messages.last() {
                    response_reasoning = vec![ProviderReasoning::from_message(message)];
                }
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    println!();
    let context_usage = latest_usage.unwrap_or(usage_total);
    Ok(PrintRunResult {
        response: full_response,
        reasoning: response_reasoning,
        usage: usage_total,
        context_usage,
        provider_calls,
    })
}

fn format_tool_args_summary(args_json: &serde_json::Value) -> String {
    match args_json {
        serde_json::Value::Object(obj) => {
            let first_key = [
                "path",
                "file_path",
                "pattern",
                "command",
                "description",
                "content",
                "name",
                "question",
                "prompt",
            ];
            for key in &first_key {
                if let Some(val) = obj.get(*key) {
                    let s = match val {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let truncated: String = if s.len() > 120 {
                        format!("{}...", &s[..117])
                    } else {
                        s
                    };
                    return truncated.to_string();
                }
            }
            String::new()
        }
        _ => format!("{}", args_json),
    }
}

#[cfg(test)]
mod usage_tests {
    use super::{TokenUsage, done_usages};

    #[test]
    fn done_usages_keep_cumulative_billing_but_latest_context() {
        let first_call = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 100,
            cached_input_tokens: 200,
            reasoning_tokens: 50,
            ..Default::default()
        };
        let second_call = TokenUsage {
            input_tokens: 1_500,
            output_tokens: 50,
            cached_input_tokens: 300,
            reasoning_tokens: 25,
            ..Default::default()
        };
        let mut total = TokenUsage::default();
        total += first_call;
        total += second_call;

        let (billing, context) = done_usages(total, Some(second_call), TokenUsage::default());

        assert_eq!(billing.input_tokens, 2_500);
        assert_eq!(billing.output_tokens, 150);
        assert_eq!(billing.cached_input_tokens, 500);
        assert_eq!(billing.reasoning_tokens, 75);
        assert_eq!(context, second_call);
    }

    #[test]
    fn done_usages_falls_back_to_final_response_usage_without_call_events() {
        let final_usage = TokenUsage {
            input_tokens: 42,
            output_tokens: 7,
            ..Default::default()
        };

        let (billing, context) = done_usages(TokenUsage::default(), None, final_usage);

        assert_eq!(billing, final_usage);
        assert_eq!(context, final_usage);
    }
}

/// Run an agent silently (no stdout/stderr printing), collecting the full
/// response text. Used by subagent tasks.
#[cfg(feature = "subagents")]
#[derive(Clone, Copy, Debug)]
pub struct SubagentLimits {
    pub context_window: u64,
    pub cutoff_fraction: f64,
    pub timeout_cutoff: Option<Duration>,
}

#[cfg(feature = "subagents")]
impl SubagentLimits {
    pub fn cutoff_tokens(self) -> Option<u64> {
        (self.context_window > 0 && self.cutoff_fraction > 0.0 && self.cutoff_fraction <= 1.0)
            .then_some((self.context_window as f64 * self.cutoff_fraction).floor() as u64)
    }
}

#[cfg(feature = "subagents")]
const SUBAGENT_INVALID_TOOL_RETRIES: usize = 2;

#[cfg(feature = "subagents")]
#[derive(Clone, Copy)]
struct SubagentPromptHook;

#[cfg(feature = "subagents")]
impl AgentHook for SubagentPromptHook {
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        context: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        Some(InvalidToolCallAction::retry(
            subagent_invalid_tool_feedback(&context.tool_name, &context.allowed_tools),
        ))
    }
}

#[cfg(feature = "subagents")]
fn subagent_invalid_tool_feedback(tool_name: &str, allowed_tools: &[String]) -> String {
    let tools = if allowed_tools.is_empty() {
        "none".to_string()
    } else {
        allowed_tools.join(", ")
    };
    format!(
        "Unknown tool `{tool_name}`. Available tools: {tools}. Retry using only an available tool, or answer without calling a tool."
    )
}

#[cfg(feature = "subagents")]
pub async fn run_subagent(
    agent: &Agent,
    prompt: &str,
    max_turns: usize,
    event_tx: Option<&mpsc::Sender<AgentEvent>>,
    limits: Option<SubagentLimits>,
) -> anyhow::Result<String> {
    let mut retry_prompt = prompt.to_string();
    let mut retry_history = Vec::<Message>::new();
    let mut tool_interactions = Vec::<Message>::new();
    let mut stream = agent
        .prompt(retry_prompt.clone())
        .history(retry_history.clone())
        .add_hook(SubagentPromptHook)
        .max_invalid_tool_call_retries(SUBAGENT_INVALID_TOOL_RETRIES)
        .max_turns(max_turns)
        .stream();

    let mut full_response = String::new();
    let mut preserved_response = String::new();
    let mut tool_notes: Vec<String> = Vec::new();
    let mut retry_attempts = 0;
    let mut stream_had_output = false;
    let timeout_deadline =
        limits.and_then(|limits| limits.timeout_cutoff.map(|d| Instant::now() + d));

    loop {
        let item = if let Some(deadline) = timeout_deadline {
            tokio::select! {
                item = stream.next() => item,
                _ = sleep_until(deadline) => {
                    return subagent_timeout_cutoff_response(
                        agent,
                        prompt,
                        &format!("{preserved_response}{full_response}"),
                        &tool_notes,
                    ).await;
                }
            }
        } else {
            stream.next().await
        };
        let Some(item) = item else { break };
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                text,
                ..
            }))) => {
                stream_had_output |= !text.is_empty();
                full_response.push_str(&text);
            }
            Ok(MultiTurnStreamItem::ToolCall { tool_call }) => {
                stream_had_output = true;
                push_tool_call(&mut tool_interactions, tool_call.clone());
                tool_notes.push(format!("called tool `{}`", tool_call.function.name));
                if let Some(tx) = event_tx {
                    let _ = tx
                        .send(AgentEvent::SubagentToolCall {
                            name: CompactString::new(tool_call.function.name.as_str()),
                            args: serde_json::Value::Object(tool_call.function.arguments),
                        })
                        .await;
                }
            }
            Ok(MultiTurnStreamItem::ToolResult { tool_result }) => {
                let mut output = String::new();
                for c in tool_result.content.iter() {
                    if let ToolResultContent::Text(t) = c {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(&t.text);
                    }
                }
                stream_had_output = true;
                tool_interactions.push(tool_result.clone().into());
                if !output.is_empty() {
                    tool_notes.push(format!(
                        "tool result: {}",
                        truncate_for_subagent_note(&output)
                    ));
                }
            }
            Ok(MultiTurnStreamItem::CompletionCall(call)) => {
                let usage = TokenUsage::from(call.usage);
                if let Some(limits) = limits
                    && let Some(cutoff) = limits.cutoff_tokens()
                    && usage.context_tokens() >= cutoff
                {
                    return subagent_context_cutoff_response(
                        agent,
                        prompt,
                        &format!("{preserved_response}{full_response}"),
                        &tool_notes,
                        usage.context_tokens(),
                        limits.context_window,
                    )
                    .await;
                }
            }
            Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                full_response = res.output().to_string();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                if let Some(delay_ms) = retry_delay_ms(retry_attempts, &e) {
                    retry_attempts = retry_attempts.saturating_add(1);
                    if stream_had_output {
                        preserved_response.push_str(&full_response);
                        prepare_retry_continuation(
                            &mut retry_history,
                            &mut retry_prompt,
                            &mut tool_interactions,
                            &full_response,
                        );
                    }
                    full_response.clear();
                    stream_had_output = false;
                    sleep_for_retry(delay_ms).await;
                    stream = agent
                        .prompt(retry_prompt.clone())
                        .history(retry_history.clone())
                        .add_hook(SubagentPromptHook)
                        .max_invalid_tool_call_retries(SUBAGENT_INVALID_TOOL_RETRIES)
                        .max_turns(max_turns)
                        .stream();
                    continue;
                }
                return Err(anyhow::anyhow!("subagent error: {}", e));
            }
        }
    }

    preserved_response.push_str(&full_response);
    if preserved_response.is_empty() {
        anyhow::bail!("subagent returned empty response");
    }

    Ok(preserved_response)
}

#[cfg(feature = "subagents")]
fn truncate_for_subagent_note(text: &str) -> String {
    const MAX: usize = 600;
    let mut out = text.chars().take(MAX).collect::<String>();
    if text.chars().count() > MAX {
        out.push_str("...");
    }
    out.replace('\n', " ")
}

#[cfg(feature = "subagents")]
async fn subagent_context_cutoff_response(
    agent: &Agent,
    original_prompt: &str,
    partial_response: &str,
    tool_notes: &[String],
    context_tokens: u64,
    context_window: u64,
) -> anyhow::Result<String> {
    let prompt = subagent_cutoff_prompt(
        original_prompt,
        partial_response,
        tool_notes,
        &format!("has reached {context_tokens}/{context_window} context tokens (>=90%)"),
    );
    let response = run_subagent_cutoff_stream(agent, prompt).await?;
    if response.trim().is_empty() {
        anyhow::bail!(
            "subagent reached {context_tokens}/{context_window} context tokens and returned empty cutoff response"
        );
    }
    Ok(response)
}

#[cfg(feature = "subagents")]
async fn subagent_timeout_cutoff_response(
    agent: &Agent,
    original_prompt: &str,
    partial_response: &str,
    tool_notes: &[String],
) -> anyhow::Result<String> {
    let prompt = subagent_cutoff_prompt(
        original_prompt,
        partial_response,
        tool_notes,
        "has reached 90% of its timeout budget",
    );
    let response = run_subagent_cutoff_stream(agent, prompt).await?;
    if response.trim().is_empty() {
        anyhow::bail!("subagent reached timeout cutoff and returned empty cutoff response");
    }
    Ok(response)
}

#[cfg(feature = "subagents")]
async fn run_subagent_cutoff_stream(agent: &Agent, prompt: String) -> anyhow::Result<String> {
    let mut retry_attempts = 0;
    'retry: loop {
        let mut stream = agent
            .prompt(prompt.clone())
            .history(Vec::<Message>::new())
            .stream();
        let mut response = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                    text,
                    ..
                }))) => response.push_str(&text),
                Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                    if !res.output().is_empty() {
                        response = res.output().to_string();
                    }
                    return Ok(response);
                }
                Ok(_) => {}
                Err(e) => {
                    if let Some(delay_ms) = retry_delay_ms(retry_attempts, &e) {
                        retry_attempts = retry_attempts.saturating_add(1);
                        sleep_for_retry(delay_ms).await;
                        continue 'retry;
                    }
                    return Err(anyhow::anyhow!("subagent cutoff response error: {}", e));
                }
            }
        }
        return Ok(response);
    }
}

#[cfg(feature = "subagents")]
fn subagent_cutoff_prompt(
    original_prompt: &str,
    partial_response: &str,
    tool_notes: &[String],
    reason: &str,
) -> String {
    format!(
        "You are a subagent that {reason}. Tool use is now disabled. Do not call tools. Answer immediately using only the partial information below. Be explicit about uncertainty and missing verification.\n\nOriginal task:\n{original_prompt}\n\nPartial assistant response so far:\n{}\n\nRecent tool trace:\n{}\n\nNow provide the best concise final answer possible from this partial information. Do not ask to continue and do not use tools.",
        partial_response.trim(),
        recent_tool_notes(tool_notes),
    )
}

#[cfg(feature = "subagents")]
fn recent_tool_notes(tool_notes: &[String]) -> String {
    if tool_notes.is_empty() {
        return "(none captured)".to_string();
    }
    tool_notes
        .iter()
        .rev()
        .take(12)
        .rev()
        .map(|note| format!("- {note}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "subagents")]
    use super::{SubagentLimits, subagent_cutoff_prompt, subagent_invalid_tool_feedback};
    use super::{
        convert_history, prepare_retry_continuation, push_tool_call, retry_delay_ms,
        streamed_assistant_content_has_output, streamed_provider_reasoning,
        streamed_reasoning_text,
    };
    use crate::session::assistant_message_with_reasoning;
    use crate::session::{MessageRole, ProviderReasoning, ProviderReasoningContent, Session};
    use rig::completion::message::{
        AssistantContent, Reasoning, ToolCall, ToolFunction, UserContent,
    };
    use rig::completion::{Message, PromptError};
    use rig::error::ProviderError;
    use rig::streaming::{Item, StreamEvent};

    fn provider_retry_delay(attempt: usize, message: &str) -> Option<u64> {
        retry_delay_ms(
            attempt,
            &PromptError::Provider(ProviderError::Provider(message.to_string())),
        )
    }

    #[cfg(feature = "subagents")]
    #[test]
    fn subagent_limits_compute_cutoff_tokens() {
        let limits = SubagentLimits {
            context_window: 1000,
            cutoff_fraction: 0.90,
            timeout_cutoff: None,
        };

        assert_eq!(limits.cutoff_tokens(), Some(900));
    }

    #[cfg(feature = "subagents")]
    #[test]
    fn subagent_invalid_tool_feedback_lists_allowed_tools() {
        let feedback =
            subagent_invalid_tool_feedback("bash", &["read".to_string(), "grep".to_string()]);

        assert!(feedback.contains("Unknown tool `bash`"));
        assert!(feedback.contains("read, grep"));
        assert!(feedback.contains("Retry"));
    }

    #[cfg(feature = "subagents")]
    #[test]
    fn subagent_invalid_tool_feedback_handles_no_allowed_tools() {
        let feedback = subagent_invalid_tool_feedback("bash", &[]);

        assert!(feedback.contains("Available tools: none"));
        assert!(feedback.contains("answer without calling a tool"));
    }

    #[cfg(feature = "subagents")]
    #[test]
    fn subagent_cutoff_prompt_commands_answer_without_tools() {
        let prompt = subagent_cutoff_prompt(
            "find the thing",
            "found one clue",
            &[
                "called tool `grep`".to_string(),
                "tool result: match".to_string(),
            ],
            "has reached 900/1000 context tokens (>=90%)",
        );

        assert!(prompt.contains("Tool use is now disabled"));
        assert!(prompt.contains("Do not call tools"));
        assert!(prompt.contains("found one clue"));
        assert!(prompt.contains("called tool `grep`"));
    }

    #[cfg(feature = "subagents")]
    #[test]
    fn subagent_cutoff_prompt_accepts_timeout_reason() {
        let prompt = subagent_cutoff_prompt(
            "find the thing",
            "",
            &[],
            "has reached 90% of its timeout budget",
        );

        assert!(prompt.contains("90% of its timeout budget"));
        assert!(prompt.contains("Do not call tools"));
    }

    fn stream_item(event: serde_json::Value) -> Item<StreamEvent> {
        serde_json::from_value(serde_json::json!({"item":"event", "value":event})).unwrap()
    }

    #[test]
    fn streamed_reasoning_delta_is_forwardable_as_reasoning_text() {
        let content = stream_item(
            serde_json::json!({"event":"reasoning", "part":0, "text":"thinking in progress"}),
        );
        assert_eq!(
            streamed_reasoning_text(&content).as_deref(),
            Some("thinking in progress")
        );
    }

    #[test]
    fn empty_reasoning_delta_is_ignored() {
        let content = stream_item(serde_json::json!({"event":"reasoning", "part":0, "text":""}));
        assert!(streamed_reasoning_text(&content).is_none());
        assert!(!streamed_assistant_content_has_output(&content));
        assert!(!streamed_assistant_content_has_output(&stream_item(
            serde_json::json!({"event":"text", "part":0, "text":""})
        )));
    }

    #[test]
    fn streamed_encrypted_reasoning_is_preserved() {
        let mut reasoning = Reasoning::new("short summary");
        reasoning.native = Some(rig::message::Native {
            item: serde_json::json!({"type":"reasoning", "id":"rs_1", "summary":[{"type":"summary_text", "text":"short summary"}], "encrypted_content":"enc_blob"}),
            fingerprint: rig::message::Fingerprint::of(&reasoning),
        });
        let content = stream_item(
            serde_json::json!({"event":"end", "part":0, "content":AssistantContent::Reasoning(reasoning.clone())}),
        );
        let stored = streamed_provider_reasoning(&content).unwrap();
        assert_eq!(stored.id, "rs_1");
        let replay = assistant_message_with_reasoning("", &[stored]);
        let Message::Assistant(message) = replay else {
            panic!("assistant");
        };
        assert_eq!(
            message.content,
            vec![AssistantContent::Reasoning(reasoning)]
        );
    }

    #[test]
    fn streamed_text_reasoning_without_an_id_is_preserved_for_replay() {
        let mut reasoning = Reasoning::new("hidden chain of thought");
        reasoning.native = Some(rig::message::Native {
            item: serde_json::json!({"type":"thinking", "thinking":reasoning.text, "signature":"signature"}),
            fingerprint: rig::message::Fingerprint::of(&reasoning),
        });
        let content = stream_item(
            serde_json::json!({"event":"end", "part":0, "content":AssistantContent::Reasoning(reasoning.clone())}),
        );
        let stored = streamed_provider_reasoning(&content).unwrap();
        assert!(stored.id.is_empty());
        let serialized = serde_json::to_string(&stored).unwrap();
        let stored = serde_json::from_str(&serialized).unwrap();
        let replay = assistant_message_with_reasoning("answer", &[stored]);
        let Message::Assistant(message) = replay else {
            panic!("assistant");
        };
        assert_eq!(message.content[0], AssistantContent::Reasoning(reasoning));
    }

    #[test]
    fn convert_history_replays_tool_events_as_native_messages() {
        let mut session = Session::new("openai", "gpt-5.1", 128000);
        session.add_message(MessageRole::User, "inspect it");
        session.add_tool_call_structured(
            "read",
            &serde_json::json!({ "path": "src/main.rs" }),
            "call_1",
            Some("fc_1"),
        );
        session.add_tool_result_structured("read", "file contents", "call_1", Some("fc_1"));

        let history = convert_history(&session);
        let Message::Assistant(rig::message::AssistantMessage { content, .. }) = &history[1] else {
            panic!("expected assistant tool call message");
        };
        let call_items = content.iter().collect::<Vec<_>>();
        assert!(matches!(call_items[0], AssistantContent::ToolCall(call)
            if call.id.to_string() == "call_1"
                && call.id.provider().map(|id| id.as_str()) == Some("call_1")
                && call.function.name == "read"
                && serde_json::Value::Object(call.function.arguments.clone()) == serde_json::json!({ "path": "src/main.rs" })));

        let Message::User { content } = &history[2] else {
            panic!("expected user tool result message");
        };
        let result_items = content.iter().collect::<Vec<_>>();
        assert!(matches!(result_items[0], UserContent::ToolResult(result)
            if result.call.to_string() == "call_1" && result.call.provider().map(|id| id.as_str()) == Some("call_1")));
    }

    #[test]
    fn openai_responses_history_synthesizes_missing_tool_call_ids() {
        let mut session = Session::new("openai-codex", "gpt-5.6-codex", 128000);
        session.add_tool_call_structured(
            "read",
            &serde_json::json!({ "path": "src/main.rs" }),
            "call_1",
            None,
        );
        session.add_tool_result_structured("read", "file contents", "call_1", None);

        let history = convert_history(&session);
        let Message::Assistant(rig::message::AssistantMessage { content, .. }) = &history[0] else {
            panic!("expected assistant tool call message");
        };
        assert!(
            matches!(content.first().unwrap(), AssistantContent::ToolCall(call)
            if call.id.to_string() == "call_1" && call.id.provider().map(|id| id.as_str()) == Some("call_1"))
        );

        let Message::User { content } = &history[1] else {
            panic!("expected user tool result message");
        };
        assert!(
            matches!(content.first().unwrap(), UserContent::ToolResult(result)
            if result.call.to_string() == "call_1" && result.call.provider().map(|id| id.as_str()) == Some("call_1"))
        );
    }

    #[test]
    fn convert_history_preserves_partial_text_before_tool_call() {
        let mut session = Session::new("openai", "gpt-5.1", 128000);
        session.add_message(MessageRole::User, "inspect it");
        session.add_partial_assistant_output("I found a lead.", Vec::new());
        session.add_tool_call_structured(
            "read",
            &serde_json::json!({ "path": "src/main.rs" }),
            "call_1",
            None,
        );
        session.add_tool_result_structured("read", "file contents", "call_1", None);

        let history = convert_history(&session);

        let Message::Assistant(rig::message::AssistantMessage { content, .. }) = &history[1] else {
            panic!("expected combined assistant text and tool call");
        };
        let items = content.iter().collect::<Vec<_>>();
        assert!(matches!(items[0], AssistantContent::Text(text) if text.text == "I found a lead."));
        assert!(
            matches!(items[1], AssistantContent::ToolCall(call) if call.function.name == "read")
        );
        assert!(matches!(history[2], Message::User { .. }));
    }

    #[test]
    fn convert_history_drops_legacy_text_only_tool_events() {
        let mut session = Session::new("openai", "gpt-5.1", 128000);
        session.add_message(MessageRole::ToolCall, "bash echo hi");
        session.add_message(MessageRole::ToolResult, "bash:\nhi");

        let history = convert_history(&session);
        assert!(history.is_empty());
    }

    #[test]
    fn convert_history_drops_subagent_trace_between_tool_call_and_result() {
        let mut session = Session::new("openai", "gpt-5.1", 128000);
        session.add_message(MessageRole::User, "audit it");
        session.add_tool_call_structured(
            "task",
            &serde_json::json!({ "prompts": ["inspect"] }),
            "call_1",
            None,
        );
        session.add_subagent_tool_call("read", &serde_json::json!({ "path": "src/main.rs" }));
        session.add_tool_result_structured("task", "done", "call_1", None);

        let history = convert_history(&session);
        assert_eq!(history.len(), 3);
        assert!(matches!(
            history[1],
            Message::Assistant(rig::message::AssistantMessage { .. })
        ));
        assert!(matches!(&history[2], Message::User { content }
            if matches!(content.first().unwrap(), UserContent::ToolResult(result)
                if result.call.to_string() == "call_1" && result.call.provider().map(|id| id.as_str()) == Some("call_1"))));
    }

    #[test]
    fn convert_history_drops_unfinished_tool_call() {
        let mut session = Session::new("openai", "gpt-5.1", 128000);
        session.add_message(MessageRole::User, "inspect it");
        session.add_tool_call_structured(
            "read",
            &serde_json::json!({ "path": "src/main.rs" }),
            "call_1",
            Some("fc_1"),
        );

        let history = convert_history(&session);
        assert_eq!(history.len(), 1);
        assert!(matches!(history[0], Message::User { .. }));
    }

    #[test]
    fn convert_history_drops_orphan_tool_result() {
        let mut session = Session::new("openai", "gpt-5.1", 128000);
        session.add_tool_result_structured("read", "file contents", "call_1", Some("fc_1"));

        let history = convert_history(&session);
        assert!(history.is_empty());
    }

    #[test]
    fn retry_classification_retries_transient_provider_failures() {
        let http_error = PromptError::Provider(ProviderError::from_http_response(
            http::StatusCode::BAD_GATEWAY,
            "bad gateway",
        ));
        assert_eq!(retry_delay_ms(0, &http_error), Some(2_000));
        assert_eq!(
            provider_retry_delay(0, "Invalid status code 503 Service Unavailable"),
            Some(2_000)
        );
        assert_eq!(
            provider_retry_delay(1, "rate limit: too many requests"),
            Some(4_000)
        );
        assert_eq!(provider_retry_delay(2, "provider_unavailable"), Some(8_000));
        assert_eq!(
            provider_retry_delay(0, "internal_error: Internal server error"),
            Some(2_000)
        );
        assert_eq!(
            provider_retry_delay(
                0,
                "ProviderError: Http client error: error decoding response body"
            ),
            Some(2_000)
        );
        assert_eq!(
            provider_retry_delay(
                0,
                "ProviderError: Http client error: error sending request for url (https://chatgpt.com/backend-api/codex/responses)"
            ),
            Some(2_000)
        );
        assert_eq!(provider_retry_delay(4, "HTTP 599"), Some(30_000));
        assert_eq!(provider_retry_delay(20, "resource_exhausted"), Some(30_000));
        assert_eq!(
            provider_retry_delay(0, "server_error: retry your request"),
            Some(2_000)
        );
        assert_eq!(
            provider_retry_delay(
                0,
                "server_is_overloaded: Our servers are currently overloaded."
            ),
            Some(2_000)
        );
    }

    #[test]
    fn parallel_tool_calls_share_one_assistant_message() {
        let mut interactions = Vec::new();
        for id in ["call_1", "call_2"] {
            push_tool_call(
                &mut interactions,
                ToolCall::new(
                    rig::message::CallId::from_wire(id),
                    ToolFunction::new(
                        rig::message::ToolName::new("read").unwrap(),
                        serde_json::json!({}),
                    ),
                ),
            );
        }

        assert_eq!(interactions.len(), 1);
        let Message::Assistant(rig::message::AssistantMessage { content, .. }) = &interactions[0]
        else {
            panic!("expected assistant tool-call message");
        };
        assert_eq!(content.len(), 2);
        assert!(
            content
                .iter()
                .all(|item| matches!(item, AssistantContent::ToolCall(_)))
        );
    }

    #[test]
    fn retry_history_keeps_every_partial_attempt() {
        let mut history = Vec::new();
        let mut prompt = "original".to_string();
        let mut interactions = Vec::new();
        prepare_retry_continuation(&mut history, &mut prompt, &mut interactions, "first half");
        prepare_retry_continuation(&mut history, &mut prompt, &mut interactions, "second half");

        assert_eq!(prompt, "Go");

        assert!(matches!(&history[0], Message::User { content } if
            matches!(content.first().unwrap(), UserContent::Text(text) if text.text == "original")));
        assert!(
            matches!(&history[1], Message::Assistant(rig::message::AssistantMessage { content, .. }) if
            matches!(content.first().unwrap(), AssistantContent::Text(text) if text.text == "first half"))
        );
        assert!(matches!(&history[2], Message::User { content } if
            matches!(content.first().unwrap(), UserContent::Text(text) if text.text == "Go")));
        assert!(
            matches!(&history[3], Message::Assistant(rig::message::AssistantMessage { content, .. }) if
            matches!(content.first().unwrap(), AssistantContent::Text(text) if text.text == "second half"))
        );
    }

    #[test]
    fn retry_classification_does_not_retry_terminal_errors() {
        let http_error = PromptError::Provider(ProviderError::from_http_response(
            http::StatusCode::BAD_REQUEST,
            "bad request",
        ));
        assert_eq!(retry_delay_ms(0, &http_error), None);
        assert_eq!(provider_retry_delay(0, "context_length_exceeded"), None);
        assert_eq!(
            provider_retry_delay(0, "No tool output found for function call"),
            None
        );
        assert_eq!(provider_retry_delay(0, "insufficient_quota"), None);
        assert_eq!(
            provider_retry_delay(0, "Invalid status code 400 Bad Request"),
            None
        );
    }

    #[test]
    fn convert_history_retransmits_provider_reasoning_before_text() {
        let mut session = Session::new("openai-codex", "gpt-5.5", 400000);
        session.add_message_with_reasoning(
            MessageRole::Assistant,
            "final answer",
            vec![ProviderReasoning {
                id: "rs_1".to_string(),
                content: vec![ProviderReasoningContent::Encrypted("enc_blob".to_string())],
            }],
        );

        let history = convert_history(&session);
        let Message::Assistant(rig::message::AssistantMessage { content, .. }) = &history[0] else {
            panic!("expected assistant message");
        };
        let items = content.iter().collect::<Vec<_>>();
        assert!(
            matches!(items[0], AssistantContent::Reasoning(reasoning) if reasoning.native.as_ref().is_some_and(|native| native.item["id"] == "rs_1"))
        );
        assert!(matches!(items[1], AssistantContent::Text(text) if text.text == "final answer"));
    }
}
