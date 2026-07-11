use std::collections::{HashMap, HashSet};
use std::time::Duration;

use compact_str::CompactString;
use futures::StreamExt;
use rig::OneOrMany;
use rig::agent::{Agent, MultiTurnStreamItem, StreamingResult};
#[cfg(feature = "subagents")]
use rig::agent::{InvalidToolCallContext, InvalidToolCallHookAction, PromptHook};
use rig::completion::message::{
    AssistantContent, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent, UserContent,
};
#[cfg(feature = "multimodal")]
use rig::completion::message::{
    AudioMediaType, Document, DocumentMediaType, DocumentSourceKind, ImageMediaType,
};
use rig::completion::{CompletionModel, Message};
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingChat};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, sleep_until};

use crate::event::{AgentEvent, BtwEvent, TokenUsage};
use crate::session::{
    MessageRole, ProviderReasoning, Session, SessionMessage, assistant_message_with_reasoning,
};

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

fn streamed_reasoning_text<R>(content: &StreamedAssistantContent<R>) -> Option<CompactString> {
    match content {
        StreamedAssistantContent::Reasoning(reasoning) => {
            Some(CompactString::new(reasoning.display_text()))
        }
        StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
            if reasoning.is_empty() {
                None
            } else {
                Some(CompactString::from(reasoning.as_str()))
            }
        }
        _ => None,
    }
}

fn streamed_provider_reasoning<R>(
    content: &StreamedAssistantContent<R>,
) -> Option<ProviderReasoning> {
    match content {
        StreamedAssistantContent::Reasoning(reasoning) => ProviderReasoning::from_rig(reasoning),
        _ => None,
    }
}

const PROVIDER_RETRY_DELAYS_MS: [u64; 3] = [1_000, 2_000, 4_000];

fn retry_delay_ms(attempt: usize, message: &str) -> Option<u64> {
    if !is_retryable_provider_error(message) {
        return None;
    }
    PROVIDER_RETRY_DELAYS_MS.get(attempt).copied()
}

fn is_retryable_provider_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if is_non_retryable_provider_error(&lower) {
        return false;
    }
    [
        " 408 ",
        " 409 ",
        " 429 ",
        " 500 ",
        " 502 ",
        " 503 ",
        " 504 ",
        "timeout",
        "timed out",
        "rate limit",
        "too many requests",
        "overloaded",
        "overload",
        "temporarily unavailable",
        "provider_unavailable",
        "service unavailable",
        "gateway timeout",
        "bad gateway",
        "connection lost",
        "connection reset",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
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
        "insufficient_quota",
        "quota",
        "billing",
        "usage limit",
        "freeusagelimiterror",
        "gousagelimiterror",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

async fn sleep_for_retry(delay_ms: u64) {
    sleep(Duration::from_millis(delay_ms)).await;
}

/// Spawn an isolated, single-turn, tool-less side-question run. The full result
/// is delivered as a single [`BtwEvent::Done`] (or [`BtwEvent::Error`]) tagged
/// with `id`. Unlike [`spawn_agent`], it never registers a subagent event sink
/// and never mutates the session.
pub fn spawn_btw<M, P>(
    agent: Agent<M, P>,
    prompt: String,
    history: Vec<Message>,
    event_tx: mpsc::Sender<BtwEvent>,
    id: u32,
) -> BtwRunner
where
    M: CompletionModel + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
    P: rig::agent::PromptHook<M> + 'static,
{
    let join = tokio::spawn(async move {
        let mut stream = agent.stream_chat(prompt, history).await;
        let mut acc = String::new();

        while let Some(item) = stream.next().await {
            match item {
                Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(
                    text,
                ))) => acc.push_str(&text.text),
                Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                    let response_text = res.response();
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
    let mut replayed_tool_call_ids = HashSet::new();

    for msg in &session.messages[first_kept..] {
        match msg.role {
            MessageRole::User => messages.push(Message::user(msg.content.to_string())),
            MessageRole::Assistant => messages.push(assistant_message_with_reasoning(
                &msg.content,
                &msg.provider_reasoning,
            )),
            // Convert non-user transcript records to Assistant for the
            // same reason as the summary above: the templates that reject
            // mid-stream System/tool roles tolerate Assistant, and code-symmetry with
            // the summary push keeps the resumed-conversation shape
            // consistent.
            MessageRole::System => messages.push(Message::assistant(msg.content.to_string())),
            MessageRole::ToolCall => {
                if let Some(call) = msg.tool_call.as_ref()
                    && replayed_tool_result_ids.contains(call.id.as_str())
                    && let Some(message) = tool_call_message(msg)
                {
                    replayed_tool_call_ids.insert(call.id.to_string());
                    messages.push(message);
                }
            }
            MessageRole::ToolResult => {
                if let Some(result) = msg.tool_result.as_ref()
                    && replayed_tool_call_ids.contains(result.id.as_str())
                    && let Some(message) = tool_result_message(msg)
                {
                    messages.push(message);
                }
            }
            MessageRole::SubagentToolCall => messages.push(Message::assistant(format!(
                "[SubagentToolCall]: {}",
                msg.content
            ))),
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

fn tool_call_message(msg: &SessionMessage) -> Option<Message> {
    let call = msg.tool_call.as_ref()?;
    Some(Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: call.id.to_string(),
            call_id: call.call_id.as_ref().map(ToString::to_string),
            function: ToolFunction::new(call.name.to_string(), call.arguments.clone()),
            signature: None,
            additional_params: None,
        })),
    })
}

fn tool_result_message(msg: &SessionMessage) -> Option<Message> {
    let result = msg.tool_result.as_ref()?;
    Some(Message::User {
        content: OneOrMany::one(UserContent::ToolResult(ToolResult {
            id: result.id.to_string(),
            call_id: result.call_id.as_ref().map(ToString::to_string),
            content: OneOrMany::one(ToolResultContent::Text(Text::new(tool_result_output(msg)))),
        })),
    })
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
    use rig::OneOrMany;
    use rig::completion::message::UserContent;

    media
        .iter()
        .map(|m| match m {
            crate::extras::multimodal::MediaAttachment::Image { data, mime, .. } => Message::User {
                content: OneOrMany::one(UserContent::image_base64(
                    BASE64_STANDARD.encode(data),
                    Some(image_media_type(mime)),
                    None,
                )),
            },
            crate::extras::multimodal::MediaAttachment::Audio { data, mime, .. } => Message::User {
                content: OneOrMany::one(UserContent::audio(
                    BASE64_STANDARD.encode(data),
                    Some(audio_media_type(mime)),
                )),
            },
            crate::extras::multimodal::MediaAttachment::Document { data, mime, .. } => {
                Message::User {
                    content: OneOrMany::one(UserContent::Document(Document {
                        data: DocumentSourceKind::Base64(BASE64_STANDARD.encode(data)),
                        media_type: Some(document_media_type(mime)),
                        additional_params: None,
                    })),
                }
            }
        })
        .collect()
}

#[cfg(feature = "multimodal")]
fn image_media_type(mime: &str) -> ImageMediaType {
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

async fn continue_prompt_injector<M, P>(
    agent: &Agent<M, P>,
    retry_prompt: &str,
    retry_history: &[Message],
    tool_interactions: &[Message],
) -> StreamingResult<M::StreamingResponse>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
    P: rig::agent::PromptHook<M> + 'static,
{
    let mut new_history = retry_history.to_vec();
    new_history.extend_from_slice(tool_interactions);
    new_history.push(Message::user(retry_prompt.to_string()));
    new_history.push(Message::assistant(String::new()));
    agent.stream_chat("Please continue.", new_history).await
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

pub fn spawn_agent<M, P>(agent: Agent<M, P>, prompt: String, history: Vec<Message>) -> AgentRunner
where
    M: CompletionModel + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
    P: rig::agent::PromptHook<M> + 'static,
{
    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(32);

    #[cfg(feature = "subagents")]
    crate::extras::subagents::set_subagent_event_tx(event_tx.clone());

    let join = tokio::spawn(async move {
        let retry_prompt = prompt.clone();
        let retry_history: Vec<Message> = history.clone();
        let mut tool_interactions: Vec<Message> = Vec::new();
        let mut last_tool_name: Option<String> = None;
        let mut tool_names: HashMap<String, String> = HashMap::new();
        let mut tool_starts: HashMap<String, Instant> = HashMap::new();
        let mut usage_total = TokenUsage::default();
        let mut latest_usage: Option<TokenUsage> = None;
        let mut response_reasoning: Vec<ProviderReasoning> = Vec::new();
        let mut retry_attempts = 0usize;
        let mut stream_had_output = false;

        let mut stream = agent.stream_chat(prompt, history).await;

        loop {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(MultiTurnStreamItem::StreamAssistantItem(content)) => {
                        stream_had_output = true;
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
                            StreamedAssistantContent::Text(text) => {
                                let _ = event_tx
                                    .send(AgentEvent::Token(CompactString::from(text.text)))
                                    .await;
                            }
                            StreamedAssistantContent::ToolCall { tool_call, .. } => {
                                response_reasoning.clear();
                                last_tool_name = Some(tool_call.function.name.clone());
                                tool_names
                                    .insert(tool_call.id.clone(), tool_call.function.name.clone());
                                tool_starts.insert(tool_call.id.clone(), Instant::now());
                                tool_interactions.push(tool_call.clone().into());
                                let _ = event_tx
                                    .send(AgentEvent::ToolCall {
                                        id: CompactString::from(tool_call.id),
                                        call_id: tool_call.call_id.map(CompactString::from),
                                        name: CompactString::from(tool_call.function.name),
                                        args: tool_call.function.arguments,
                                    })
                                    .await;
                            }
                            _ => {}
                        }
                    }
                    Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                        tool_result,
                        ..
                    })) => {
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
                        let name = tool_names
                            .remove(&tool_result.id)
                            .or_else(|| last_tool_name.take())
                            .unwrap_or_default();
                        let loaded_context = if name == "read" {
                            crate::agent::tools::take_read_context_metadata(&output)
                        } else {
                            Vec::new()
                        };
                        let duration_ms = tool_starts
                            .remove(&tool_result.id)
                            .map(|start| start.elapsed().as_millis().try_into().unwrap_or(u64::MAX))
                            .unwrap_or(0);
                        let display_artifact = if name == "edit" {
                            crate::agent::tools::edit::take_last_edit_display_artifact()
                        } else {
                            None
                        };
                        let _ = event_tx
                            .send(AgentEvent::ToolResult {
                                id: CompactString::new(tool_result.id.clone()),
                                call_id: tool_result.call_id.clone().map(CompactString::from),
                                name: CompactString::new(name),
                                output: CompactString::from(output),
                                loaded_context,
                                duration_ms,
                                display_artifact,
                            })
                            .await;
                        tool_interactions.push(tool_result.clone().into());
                    }
                    Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                        let response_text = res.response();
                        let final_usage = res.usage().into();

                        if !response_text.is_empty() {
                            let (usage, context_usage) =
                                done_usages(usage_total, latest_usage, final_usage);
                            let reasoning = std::mem::take(&mut response_reasoning);
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
                        let usage = TokenUsage::from(call.usage);
                        usage_total += usage;
                        latest_usage = Some(usage);
                        let _ = event_tx
                            .send(AgentEvent::CompletionCall {
                                call_index: call.call_index,
                                usage,
                            })
                            .await;
                    }
                    Err(e) => {
                        let message = e.to_string();
                        if !stream_had_output
                            && let Some(delay_ms) = retry_delay_ms(retry_attempts, &message)
                        {
                            retry_attempts += 1;
                            let _ = event_tx
                                .send(AgentEvent::Retry {
                                    attempt: retry_attempts as u32,
                                    delay_ms,
                                    message: CompactString::new(message),
                                })
                                .await;
                            sleep_for_retry(delay_ms).await;
                            stream = agent
                                .stream_chat(retry_prompt.clone(), retry_history.clone())
                                .await;
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
                    _ => {}
                }
            }

            retry_attempts = 0;
            stream_had_output = false;
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

pub async fn run_print<M, P>(
    agent: &Agent<M, P>,
    prompt: &str,
    max_turns: usize,
    pure_stdout: bool,
) -> anyhow::Result<PrintRunResult>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
    P: rig::agent::PromptHook<M> + 'static,
{
    let mut stream = agent
        .stream_chat(prompt.to_string(), Vec::<Message>::new())
        .multi_turn(max_turns)
        .await;

    let mut full_response = String::new();
    let mut response_reasoning = Vec::new();
    let mut usage_total = TokenUsage::default();
    let mut latest_usage: Option<TokenUsage> = None;
    let mut last_tool_name: Option<String> = None;

    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text))) => {
                full_response.push_str(&text.text);
                print!("{}", text.text);
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Reasoning(
                r,
            ))) => {
                eprint!("{}", r.display_text());
                if let Some(reasoning) = ProviderReasoning::from_rig(&r) {
                    response_reasoning.push(reasoning);
                }
                let _ = std::io::Write::flush(&mut std::io::stderr());
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            })) if pure_stdout => {
                let name = &tool_call.function.name;
                last_tool_name = Some(name.clone());
                let summary = format_tool_args_summary(&tool_call.function.arguments);
                println!("\n◈ {} {}", name, summary);
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                ..
            })) if pure_stdout => {
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
            Ok(MultiTurnStreamItem::CompletionCall(call)) => {
                let usage = TokenUsage::from(call.usage);
                usage_total += usage;
                latest_usage = Some(usage);
            }
            Ok(MultiTurnStreamItem::FinalResponse(_)) => break,
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
impl<M: CompletionModel> PromptHook<M> for SubagentPromptHook {
    async fn on_invalid_tool_call(
        &self,
        context: &InvalidToolCallContext,
    ) -> InvalidToolCallHookAction {
        InvalidToolCallHookAction::retry(subagent_invalid_tool_feedback(
            &context.tool_name,
            &context.allowed_tools,
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
pub async fn run_subagent<M, P>(
    agent: &Agent<M, P>,
    prompt: &str,
    max_turns: usize,
    event_tx: Option<&mpsc::Sender<AgentEvent>>,
    limits: Option<SubagentLimits>,
) -> anyhow::Result<String>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
    P: rig::agent::PromptHook<M> + 'static,
{
    let mut stream = agent
        .stream_chat(prompt.to_string(), Vec::<Message>::new())
        .with_hook(SubagentPromptHook)
        .max_invalid_tool_call_retries(SUBAGENT_INVALID_TOOL_RETRIES)
        .multi_turn(max_turns)
        .await;

    let mut full_response = String::new();
    let mut tool_notes: Vec<String> = Vec::new();
    let timeout_deadline =
        limits.and_then(|limits| limits.timeout_cutoff.map(|d| Instant::now() + d));

    loop {
        let item = if let Some(deadline) = timeout_deadline {
            tokio::select! {
                item = stream.next() => item,
                _ = sleep_until(deadline) => {
                    return subagent_timeout_cutoff_response(agent, prompt, &full_response, &tool_notes).await;
                }
            }
        } else {
            stream.next().await
        };
        let Some(item) = item else { break };
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text))) => {
                full_response.push_str(&text.text);
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            })) => {
                tool_notes.push(format!("called tool `{}`", tool_call.function.name));
                if let Some(tx) = event_tx {
                    let _ = tx
                        .send(AgentEvent::SubagentToolCall {
                            name: CompactString::from(tool_call.function.name),
                            args: tool_call.function.arguments,
                        })
                        .await;
                }
            }
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                ..
            })) => {
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
                        &full_response,
                        &tool_notes,
                        usage.context_tokens(),
                        limits.context_window,
                    )
                    .await;
                }
            }
            Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                full_response = res.response().to_string();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                return Err(anyhow::anyhow!("subagent error: {}", e));
            }
        }
    }

    if full_response.is_empty() {
        anyhow::bail!("subagent returned empty response");
    }

    Ok(full_response)
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
async fn subagent_context_cutoff_response<M, P>(
    agent: &Agent<M, P>,
    original_prompt: &str,
    partial_response: &str,
    tool_notes: &[String],
    context_tokens: u64,
    context_window: u64,
) -> anyhow::Result<String>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
    P: rig::agent::PromptHook<M> + 'static,
{
    let prompt = subagent_cutoff_prompt(
        original_prompt,
        partial_response,
        tool_notes,
        &format!("has reached {context_tokens}/{context_window} context tokens (>=90%)"),
    );
    let mut stream = agent.stream_chat(prompt, Vec::<Message>::new()).await;
    let mut response = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text))) => {
                response.push_str(&text.text);
            }
            Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                let final_response = res.response();
                if !final_response.is_empty() {
                    response = final_response.to_string();
                }
                break;
            }
            Ok(_) => {}
            Err(e) => return Err(anyhow::anyhow!("subagent cutoff response error: {}", e)),
        }
    }
    if response.trim().is_empty() {
        anyhow::bail!(
            "subagent reached {context_tokens}/{context_window} context tokens and returned empty cutoff response"
        );
    }
    Ok(response)
}

#[cfg(feature = "subagents")]
async fn subagent_timeout_cutoff_response<M, P>(
    agent: &Agent<M, P>,
    original_prompt: &str,
    partial_response: &str,
    tool_notes: &[String],
) -> anyhow::Result<String>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
    P: rig::agent::PromptHook<M> + 'static,
{
    let prompt = subagent_cutoff_prompt(
        original_prompt,
        partial_response,
        tool_notes,
        "has reached 90% of its timeout budget",
    );
    let mut stream = agent.stream_chat(prompt, Vec::<Message>::new()).await;
    let mut response = String::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text))) => {
                response.push_str(&text.text);
            }
            Ok(MultiTurnStreamItem::FinalResponse(res)) => {
                let final_response = res.response();
                if !final_response.is_empty() {
                    response = final_response.to_string();
                }
                break;
            }
            Ok(_) => {}
            Err(e) => return Err(anyhow::anyhow!("subagent cutoff response error: {}", e)),
        }
    }
    if response.trim().is_empty() {
        anyhow::bail!("subagent reached timeout cutoff and returned empty cutoff response");
    }
    Ok(response)
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
        convert_history, retry_delay_ms, streamed_provider_reasoning, streamed_reasoning_text,
    };
    use crate::session::{MessageRole, ProviderReasoning, ProviderReasoningContent, Session};
    use rig::completion::Message;
    use rig::completion::message::{AssistantContent, Reasoning, ReasoningContent, UserContent};
    use rig::streaming::StreamedAssistantContent;

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

    #[test]
    fn streamed_reasoning_delta_is_forwardable_as_reasoning_text() {
        let content = StreamedAssistantContent::<()>::ReasoningDelta {
            id: Some("rs_demo".to_string()),
            reasoning: "thinking in progress".to_string(),
        };

        assert_eq!(
            streamed_reasoning_text(&content).as_deref(),
            Some("thinking in progress")
        );
    }

    #[test]
    fn empty_reasoning_delta_is_ignored() {
        let content = StreamedAssistantContent::<()>::ReasoningDelta {
            id: None,
            reasoning: String::new(),
        };

        assert!(streamed_reasoning_text(&content).is_none());
    }

    #[test]
    fn streamed_encrypted_reasoning_is_preserved() {
        let mut reasoning =
            Reasoning::summaries(vec!["short summary".to_string()]).with_id("rs_1".to_string());
        reasoning
            .content
            .push(ReasoningContent::Encrypted("enc_blob".to_string()));
        let content = StreamedAssistantContent::<()>::Reasoning(reasoning);

        let stored = streamed_provider_reasoning(&content).unwrap();
        assert_eq!(stored.id, "rs_1");
        assert_eq!(
            stored.content,
            vec![
                ProviderReasoningContent::Summary("short summary".to_string()),
                ProviderReasoningContent::Encrypted("enc_blob".to_string()),
            ]
        );
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
        let Message::Assistant { content, .. } = &history[1] else {
            panic!("expected assistant tool call message");
        };
        let call_items = content.iter().collect::<Vec<_>>();
        assert!(matches!(call_items[0], AssistantContent::ToolCall(call)
            if call.id == "call_1"
                && call.call_id.as_deref() == Some("fc_1")
                && call.function.name == "read"
                && call.function.arguments == serde_json::json!({ "path": "src/main.rs" })));

        let Message::User { content } = &history[2] else {
            panic!("expected user tool result message");
        };
        let result_items = content.iter().collect::<Vec<_>>();
        assert!(matches!(result_items[0], UserContent::ToolResult(result)
            if result.id == "call_1" && result.call_id.as_deref() == Some("fc_1")));
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
        assert_eq!(
            retry_delay_ms(0, "Invalid status code 503 Service Unavailable"),
            Some(1_000)
        );
        assert_eq!(
            retry_delay_ms(1, "rate limit: too many requests"),
            Some(2_000)
        );
        assert_eq!(retry_delay_ms(2, "provider_unavailable"), Some(4_000));
        assert_eq!(retry_delay_ms(3, "provider_unavailable"), None);
    }

    #[test]
    fn retry_classification_does_not_retry_terminal_errors() {
        assert_eq!(retry_delay_ms(0, "context_length_exceeded"), None);
        assert_eq!(
            retry_delay_ms(0, "No tool output found for function call"),
            None
        );
        assert_eq!(retry_delay_ms(0, "insufficient_quota"), None);
        assert_eq!(
            retry_delay_ms(0, "Invalid status code 400 Bad Request"),
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
        let Message::Assistant { content, .. } = &history[0] else {
            panic!("expected assistant message");
        };
        let items = content.iter().collect::<Vec<_>>();
        assert!(
            matches!(items[0], AssistantContent::Reasoning(reasoning) if reasoning.id.as_deref() == Some("rs_1"))
        );
        assert!(matches!(items[1], AssistantContent::Text(text) if text.text == "final answer"));
    }
}
