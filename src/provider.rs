use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use compact_str::CompactString;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use rig::agent::Agent;
use rig::client::{CompletionClient, ModelListingClient};
use rig::completion::{CompletionModel, Message};
use rig::http_client;
use rig::http_client::{HttpClientExt, MultipartForm};
use rig::providers::{anthropic, gemini, ollama, openai, openrouter};
use rig::streaming::StreamingChat;
use tokio::sync::mpsc;

use crate::agent::builder;
use crate::agent::prompt;
use crate::agent::runner::{self, AgentRunner};
use crate::auth::{AuthResolver, ProviderKind};
use crate::cli::Cli;
use crate::config::{ApiStyle, Config, CustomProviderConfig};
use crate::context::ContextFiles;
use crate::event::AgentEvent;
#[cfg(feature = "mcp")]
use crate::extras::mcp::McpClientManager;
use crate::permission::ask::AskSender;
use crate::permission::checker::PermCheck;
use crate::sandbox::Sandbox;
use crate::session::SessionMessage;

const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub api_key_env: Option<CompactString>,
    pub danger_accept_invalid_certs: bool,
}

pub fn resolve_provider_config(
    name: &str,
    custom_providers: &HashMap<String, CustomProviderConfig>,
) -> anyhow::Result<ProviderConfig> {
    if let Some(custom) = custom_providers.get(name) {
        let kind = ProviderKind::from_name(&custom.provider_type)
            .ok_or_else(|| anyhow::anyhow!("Unknown provider type: {}", custom.provider_type))?;
        return Ok(ProviderConfig {
            kind,
            base_url: Some(custom.base_url.clone()),
            api_key_env: custom.api_key_env.clone(),
            danger_accept_invalid_certs: custom.danger_accept_invalid_certs.unwrap_or(false),
        });
    }
    let kind = ProviderKind::from_name(name).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown provider: '{}'. Supported: openrouter, openai, openai-codex, anthropic, gemini, ollama",
            name
        )
    })?;

    Ok(ProviderConfig {
        kind,
        base_url: None,
        api_key_env: None,
        danger_accept_invalid_certs: false,
    })
}

/// Re-exported for compatibility with existing code
pub fn parse_provider(name: &str) -> Option<ProviderKind> {
    ProviderKind::from_name(name)
}

/// Pick a sensible default model when targeting `provider`. Priority:
/// a custom gateway's configured `model`, then a quick model targeting this
/// provider (carrying its pricing), then a built-in fallback. Returns
/// (model, Option<(input_cost, output_cost)>), or None if `provider` is unknown
/// and has no configured default. Used both by `/provider` and at startup so a
/// chosen provider never keeps an id that is invalid on it.
pub(crate) fn default_model_for_provider(
    provider: &str,
    cfg: &Config,
) -> Option<(String, Option<(f64, f64)>)> {
    if let Some(c) = cfg.custom_providers_map().get(provider)
        && let Some(m) = &c.model
    {
        return Some((m.to_string(), None));
    }
    // Deterministic: prefer the alphabetically-first quick model for this provider
    // (HashMap iteration order would otherwise be unstable).
    let qm = crate::config::quick_models_map(cfg);
    let mut names: Vec<&String> = qm.keys().collect();
    names.sort();
    for name in names {
        let q = &qm[name];
        if q.provider.as_str() == provider {
            return Some((
                q.model.to_string(),
                Some((q.input_token_cost, q.output_token_cost)),
            ));
        }
    }
    let m = match provider {
        "anthropic" => "claude-sonnet-4-6",
        "openai" => "gpt-5.1",
        "openai-codex" | "codex" => "gpt-5.5",
        "gemini" | "google" => "gemini-2.5-pro",
        "openrouter" => "openrouter/auto", // OpenRouter's always-valid auto-router
        "ollama" => "llama3.1",
        _ => return None,
    };
    Some((m.to_string(), None))
}

fn resolve_base_url(config: &ProviderConfig) -> Option<String> {
    config.base_url.clone()
}

/// rig 0.37 exposes two distinct OpenAI client types:
/// - `openai::Client`            -> Responses API (`/responses`). Real OpenAI,
///   including GPT-5; rig maps `max_tokens` to `max_output_tokens`, so it does
///   not hit the GPT-5 400.
/// - `openai::CompletionsClient` -> Chat Completions API (`/chat/completions`).
///   Most OpenAI-compatible gateways (vLLM / LiteLLM / self-hosted) implement
///   only this endpoint.
///
/// The two cannot share a single type, so we wrap them in an inner enum and let
/// `ApiStyle` decide which one to build.
#[derive(Clone)]
pub enum OpenAiClient {
    Responses(openai::Client),
    Completions(openai::CompletionsClient),
    Codex(openai::Client<CodexHttpClient>),
}

impl OpenAiClient {
    fn completion_model(&self, name: String) -> OpenAiModel {
        match self {
            OpenAiClient::Responses(c) => OpenAiModel::Responses(c.completion_model(name)),
            OpenAiClient::Completions(c) => OpenAiModel::Completions(c.completion_model(name)),
            OpenAiClient::Codex(c) => OpenAiModel::Codex(c.completion_model(name)),
        }
    }
}

pub enum OpenAiModel {
    Responses(openai::responses_api::ResponsesCompletionModel),
    Completions(openai::completion::CompletionModel),
    Codex(openai::responses_api::ResponsesCompletionModel<CodexHttpClient>),
}

#[derive(Clone)]
pub enum OpenAiAgent {
    Responses(Agent<openai::responses_api::ResponsesCompletionModel>),
    Completions(Agent<openai::completion::CompletionModel>),
    Codex(Agent<openai::responses_api::ResponsesCompletionModel<CodexHttpClient>>),
}

#[derive(Clone, Debug, Default)]
pub struct CodexHttpClient {
    inner: reqwest::Client,
}

impl CodexHttpClient {
    fn new(inner: reqwest::Client) -> Self {
        Self { inner }
    }

    async fn authorize_headers(&self, headers: &mut HeaderMap) -> http_client::Result<()> {
        let auth = crate::auth::codex_request_auth()
            .await
            .map_err(to_http_error)?;
        headers.insert(
            reqwest::header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", auth.access_token))?,
        );
        headers.insert(
            HeaderName::from_static("chatgpt-account-id"),
            HeaderValue::from_str(&auth.account_id)?,
        );
        headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_static("zerostack"),
        );
        headers.insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static("responses=experimental"),
        );
        Ok(())
    }

    async fn prepare_request_body(
        &self,
        headers: &mut HeaderMap,
        body: bytes::Bytes,
    ) -> http_client::Result<bytes::Bytes> {
        self.authorize_headers(headers).await?;
        let patched = ensure_codex_instructions(body)?;
        headers.remove(reqwest::header::CONTENT_LENGTH);
        Ok(patched)
    }
}

fn ensure_codex_instructions(body: bytes::Bytes) -> http_client::Result<bytes::Bytes> {
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Ok(body);
    };
    let Some(object) = value.as_object_mut() else {
        return Ok(body);
    };
    let needs_instructions = object
        .get("instructions")
        .is_none_or(serde_json::Value::is_null);
    object.insert("store".to_string(), serde_json::Value::Bool(false));
    object.remove("max_output_tokens");
    if needs_instructions {
        let instructions = take_system_instructions(object.get_mut("input"))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "You are a helpful assistant.".to_string());
        object.insert(
            "instructions".to_string(),
            serde_json::Value::String(instructions),
        );
    }
    normalize_codex_input_content(object.get_mut("input"));
    serde_json::to_vec(&value)
        .map(bytes::Bytes::from)
        .map_err(|e| http_client::Error::Instance(Box::new(e)))
}

fn normalize_codex_input_content(input: Option<&mut serde_json::Value>) {
    let Some(items) = input.and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    for item in items {
        let role = item.get("role").and_then(|role| role.as_str());
        let expected_text_type = match role {
            Some("assistant") => "output_text",
            Some("user") | Some("developer") | Some("system") => "input_text",
            _ => continue,
        };
        let Some(content_items) = item
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for content in content_items {
            let Some(kind) = content.get_mut("type") else {
                continue;
            };
            if matches!(kind.as_str(), Some("input_text" | "output_text")) {
                *kind = serde_json::Value::String(expected_text_type.to_string());
            }
        }
    }
}

fn take_system_instructions(input: Option<&mut serde_json::Value>) -> Option<String> {
    let input = input?;
    let items = input.as_array_mut()?;
    let index = items
        .iter()
        .position(|item| item.get("role").and_then(|role| role.as_str()) == Some("system"))?;
    let text = input_item_text(&items[index])?;
    items.remove(index);
    Some(text)
}

fn input_item_text(item: &serde_json::Value) -> Option<String> {
    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    let content = item.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    if let Some(items) = content.as_array() {
        let parts: Vec<&str> = items
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| part.get("content").and_then(|v| v.as_str()))
            })
            .collect();
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}

#[derive(Debug)]
struct CodexHttpError(String);

impl std::fmt::Display for CodexHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CodexHttpError {}

fn to_http_error(error: anyhow::Error) -> http_client::Error {
    http_client::Error::Instance(Box::new(CodexHttpError(format!("{error:#}"))))
}

impl HttpClientExt for CodexHttpClient {
    fn send<T, U>(
        &self,
        req: http_client::Request<T>,
    ) -> impl Future<Output = http_client::Result<http_client::Response<http_client::LazyBody<U>>>>
    + Send
    + 'static
    where
        T: Into<bytes::Bytes> + Send,
        U: From<bytes::Bytes> + Send + 'static,
    {
        let this = self.clone();
        let (mut parts, body) = req.into_parts();
        let body = body.into();
        async move {
            let body = this.prepare_request_body(&mut parts.headers, body).await?;
            let req = http_client::Request::from_parts(parts, body);
            this.inner.send(req).await
        }
    }

    fn send_multipart<U>(
        &self,
        req: http_client::Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<http_client::Response<http_client::LazyBody<U>>>>
    + Send
    + 'static
    where
        U: From<bytes::Bytes> + Send + 'static,
    {
        let this = self.clone();
        let (mut parts, body) = req.into_parts();
        async move {
            this.authorize_headers(&mut parts.headers).await?;
            let req = http_client::Request::from_parts(parts, body);
            this.inner.send_multipart(req).await
        }
    }

    fn send_streaming<T>(
        &self,
        req: http_client::Request<T>,
    ) -> impl Future<Output = http_client::Result<http_client::StreamingResponse>> + Send
    where
        T: Into<bytes::Bytes> + Send,
    {
        let this = self.clone();
        let (mut parts, body) = req.into_parts();
        let body = body.into();
        async move {
            let body = this.prepare_request_body(&mut parts.headers, body).await?;
            let req = http_client::Request::from_parts(parts, body);
            let mut response = this.inner.send_streaming(req).await?;
            response.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                HeaderValue::from_static("text/event-stream"),
            );
            Ok(response)
        }
    }
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct TestClient {
    pub(crate) prompts: Arc<StdMutex<Vec<String>>>,
}

#[derive(Clone)]
pub enum AnyClient {
    OpenRouter(openrouter::Client),
    OpenAI(OpenAiClient),
    Anthropic(anthropic::Client),
    Gemini(gemini::Client),
    Ollama(ollama::Client),
    #[cfg(test)]
    Test(TestClient),
}

/// Extra OpenRouter request body params that pin a Claude model to the
/// Anthropic direct route, or `None` for any non-Claude model.
///
/// `cache_control` breakpoints (used for prompt caching) are only honored on
/// OpenRouter's Anthropic direct route; the Bedrock and Vertex routes silently
/// drop them. So for Claude models we force `provider.order = ["Anthropic"]`
/// (keeping `allow_fallbacks: true` so the request still succeeds if Anthropic
/// is momentarily unavailable). Every other OpenRouter model caches
/// automatically and is left untouched.
///
/// OpenRouter namespaces Claude under `anthropic/`, optionally with a leading
/// `~` marking a floating "-latest" alias (e.g. `~anthropic/claude-sonnet-latest`).
/// The `~` is part of the real slug, so strip it before matching.
pub(crate) fn openrouter_anthropic_routing(model_id: &str) -> Option<serde_json::Value> {
    let slug = model_id.strip_prefix('~').unwrap_or(model_id);
    slug.starts_with("anthropic/").then(|| {
        serde_json::json!({
            "provider": { "order": ["Anthropic"], "allow_fallbacks": true }
        })
    })
}

/// Shallow-merges user-configured `extra_body` into provider-internal routing
/// params (e.g. OpenRouter's `provider.order`). Top-level keys from `extra_body`
/// win on collision. Returns `None` when both are absent so callers can avoid an
/// empty `additional_params` call.
pub(crate) fn merge_extra_body(
    base: Option<serde_json::Value>,
    extra: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (base, extra) {
        (Some(serde_json::Value::Object(mut b)), Some(serde_json::Value::Object(e))) => {
            b.extend(e);
            Some(serde_json::Value::Object(b))
        }
        (base, None) => base,
        (None, extra) => extra,
        // Non-object base (shouldn't happen for routing) — user value takes over.
        (Some(_), extra) => extra,
    }
}

impl AnyClient {
    #[allow(dead_code)]
    pub fn provider_name(&self) -> &'static str {
        match self {
            AnyClient::OpenRouter(_) => "openrouter",
            AnyClient::OpenAI(OpenAiClient::Codex(_)) => "openai-codex",
            AnyClient::OpenAI(_) => "openai",
            AnyClient::Anthropic(_) => "anthropic",
            AnyClient::Gemini(_) => "gemini",
            AnyClient::Ollama(_) => "ollama",
            #[cfg(test)]
            AnyClient::Test(_) => "test",
        }
    }

    pub fn completion_model(&self, name: impl Into<String>) -> AnyModel {
        let name = name.into();
        match self {
            AnyClient::OpenRouter(c) => {
                let extra = openrouter_anthropic_routing(&name);
                AnyModel::OpenRouter(c.completion_model(name).with_prompt_caching(), extra)
            }
            AnyClient::OpenAI(c) => AnyModel::OpenAI(c.completion_model(name)),
            AnyClient::Anthropic(c) => {
                AnyModel::Anthropic(c.completion_model(name).with_prompt_caching())
            }
            AnyClient::Gemini(c) => AnyModel::Gemini(c.completion_model(name)),
            AnyClient::Ollama(c) => AnyModel::Ollama(c.completion_model(name)),
            #[cfg(test)]
            AnyClient::Test(c) => AnyModel::Test(c.clone()),
        }
    }

    pub async fn compress_messages(
        &self,
        model_name: &str,
        messages: &[SessionMessage],
        previous_summary: Option<&str>,
        instructions: Option<&str>,
    ) -> anyhow::Result<String> {
        let conversation = serialize_conversation(messages);

        let prompt = prompt::COMPACTION_PROMPT
            .replace("{conversation}", &conversation)
            .replace("{previous_summary}", previous_summary.unwrap_or("(none)"))
            .replace("{instructions}", instructions.unwrap_or("(none)"));

        let model = self.completion_model(model_name.to_string());
        let response = summarize_with_model(model, prompt).await?;
        Ok(response)
    }
}

#[derive(Clone)]
pub struct ModelEntry {
    pub id: String,
    pub display: String,
    pub context_length: Option<u32>,
    pub kind: Option<String>, // rig Model.r#type (often None)
}

impl ModelEntry {
    fn from_rig(m: &rig::model::listing::Model) -> Self {
        Self {
            id: m.id.clone(),
            display: m.display_name().to_string(),
            context_length: m.context_length,
            kind: m.r#type.clone(),
        }
    }
}

/// Chat/completion model suitable as an agent (not embedding/image/audio/etc.)?
pub fn is_agent_model(m: &ModelEntry) -> bool {
    if let Some(t) = m.kind.as_deref() {
        let t = t.to_lowercase();
        if [
            "embed",
            "image",
            "audio",
            "video",
            "moderation",
            "rerank",
            "tts",
            "speech",
        ]
        .iter()
        .any(|k| t.contains(k))
        {
            return false;
        }
    }
    let id = m.id.to_lowercase();
    const DENY: &[&str] = &[
        "embedding",
        "embed-",
        "text-embedding",
        "gemini-embedding",
        "whisper",
        "transcribe",
        "tts",
        "-audio",
        "realtime",
        "speech",
        "dall-e",
        "gpt-image",
        "image-generation",
        "imagen",
        "sora",
        "veo",
        "moderation",
        "rerank",
        "aqa",
        "davinci-002",
        "babbage-002",
    ];
    !DENY.iter().any(|d| id.contains(d))
}

impl AnyClient {
    /// Built-in providers: rig's ModelListingClient.
    pub async fn list_models(&self) -> anyhow::Result<Vec<ModelEntry>> {
        let list = match self {
            AnyClient::OpenAI(OpenAiClient::Responses(c)) => c.list_models().await?,
            AnyClient::Anthropic(c) => c.list_models().await?,
            AnyClient::OpenRouter(c) => c.list_models().await?,
            AnyClient::Gemini(c) => c.list_models().await?,
            AnyClient::Ollama(c) => c.list_models().await?,
            // If any arm above does NOT impl ModelListingClient it won't compile —
            // move it down here to the manual fallback.
            AnyClient::OpenAI(OpenAiClient::Completions(_)) => {
                anyhow::bail!("rig model listing unavailable for this client")
            }
            AnyClient::OpenAI(OpenAiClient::Codex(_)) => return Ok(codex_model_entries()),
            #[cfg(test)]
            AnyClient::Test(_) => return Ok(Vec::new()),
        };
        Ok(list.iter().map(ModelEntry::from_rig).collect())
    }
}

fn codex_model_entries() -> Vec<ModelEntry> {
    crate::models_catalog::catalog_entries("openai-codex")
        .unwrap_or(&[])
        .to_vec()
}

/// Custom / OpenAI-compatible gateway: best-effort GET {base}/models.
pub async fn list_models_manual(
    provider_name: &str,
    cli_key: Option<&str>,
    custom_providers: &std::collections::HashMap<String, CustomProviderConfig>,
    config_api_keys: Option<&std::collections::HashMap<String, String>>,
) -> anyhow::Result<Vec<ModelEntry>> {
    let config = resolve_provider_config(provider_name, custom_providers)?;
    let base = config
        .base_url
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no base_url"))?;
    let key = AuthResolver::new(config.kind)
        .with_cli_key(cli_key)
        .with_env_override(config.api_key_env.as_deref())
        .with_config_keys(config_api_keys)
        .with_custom_provider_name(Some(provider_name))
        .resolve()
        .ok();
    let custom = custom_providers.get(provider_name);
    let http = build_http_client(provider_name, config.danger_accept_invalid_certs, custom)?;
    let url = format!("{}/models", base.trim_end_matches('/'));
    let mut req = http.get(url);
    if let Some(k) = key.as_deref().filter(|k| !k.is_empty()) {
        req = req.bearer_auth(k);
    }
    #[derive(serde::Deserialize)]
    struct Resp {
        data: Vec<Item>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
        id: String,
    }
    let resp: Resp = req.send().await?.error_for_status()?.json().await?;
    Ok(resp
        .data
        .into_iter()
        .map(|i| ModelEntry {
            display: i.id.clone(),
            id: i.id,
            context_length: None,
            kind: None,
        })
        .collect())
}

async fn summarize_with_model(model: AnyModel, prompt: String) -> anyhow::Result<String> {
    match model {
        AnyModel::OpenRouter(m, _) => run_summarizer(m, prompt).await,
        AnyModel::OpenAI(m) => match m {
            OpenAiModel::Responses(m) => run_summarizer(m, prompt).await,
            OpenAiModel::Completions(m) => run_summarizer(m, prompt).await,
            OpenAiModel::Codex(m) => run_summarizer(m, prompt).await,
        },
        AnyModel::Anthropic(m) => run_summarizer(m, prompt).await,
        AnyModel::Gemini(m) => run_summarizer(m, prompt).await,
        AnyModel::Ollama(m) => run_summarizer(m, prompt).await,
        #[cfg(test)]
        AnyModel::Test(_) => Ok(prompt),
    }
}

async fn run_summarizer<M>(model: M, prompt: String) -> anyhow::Result<String>
where
    M: CompletionModel + 'static,
    M::StreamingResponse: Send + Sync + Unpin + Clone + 'static,
{
    let mut preamble = "You are a conversation summarizer.".to_string();
    if let Some(s) = crate::session::storage::load_suffix() {
        preamble.push_str("\n\n---\n\n");
        preamble.push_str(&s);
    }

    let agent = rig::agent::AgentBuilder::new(model)
        .preamble(&preamble)
        .build();

    let mut stream = agent
        .stream_chat(prompt, Vec::<Message>::new())
        .multi_turn(1)
        .await;

    let mut response = String::new();
    use futures::StreamExt;
    while let Some(item) = stream.next().await {
        match item {
            Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                rig::streaming::StreamedAssistantContent::Text(text),
            )) => response.push_str(&text.text),
            Ok(rig::agent::MultiTurnStreamItem::FinalResponse(res)) => {
                response = res.response().to_string();
                break;
            }
            Err(e) => return Err(anyhow::anyhow!("Compression failed: {}", e)),
            _ => {}
        }
    }

    if response.is_empty() {
        anyhow::bail!("Compression returned empty response");
    }

    Ok(response)
}

pub(crate) fn serialize_conversation(messages: &[SessionMessage]) -> String {
    let mut result = String::new();
    for msg in messages {
        let role_tag = match msg.role {
            crate::session::MessageRole::User => "User",
            crate::session::MessageRole::Assistant => "Assistant",
            crate::session::MessageRole::System => "System",
            crate::session::MessageRole::ToolCall => "ToolCall",
            crate::session::MessageRole::ToolResult => "ToolResult",
            crate::session::MessageRole::SubagentToolCall => "SubagentToolCall",
        };
        result.push_str(&format!("[{}]: {}\n\n", role_tag, msg.content));
    }
    result
}

pub enum AnyModel {
    /// The second field carries provider-specific extra body params. For
    /// `anthropic/*` models routed via OpenRouter it pins `provider.order` to
    /// the Anthropic direct route, the only route that honors `cache_control`
    /// breakpoints (Bedrock/Vertex silently drop them). `None` for every other
    /// OpenRouter model, which caches automatically and needs no routing.
    OpenRouter(
        openrouter::completion::CompletionModel,
        Option<serde_json::Value>,
    ),
    OpenAI(OpenAiModel),
    Anthropic(anthropic::completion::CompletionModel),
    Gemini(gemini::completion::CompletionModel),
    Ollama(ollama::CompletionModel),
    #[cfg(test)]
    Test(TestClient),
}

#[derive(Clone)]
pub enum AnyAgent {
    OpenRouter(Agent<openrouter::completion::CompletionModel>),
    OpenAI(OpenAiAgent),
    Anthropic(Agent<anthropic::completion::CompletionModel>),
    Gemini(Agent<gemini::completion::CompletionModel>),
    Ollama(Agent<ollama::CompletionModel>),
    #[cfg(test)]
    Test(TestAgent),
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestAgent {
    pub(crate) prompts: Arc<StdMutex<Vec<String>>>,
    pub(crate) sandbox: Sandbox,
}

impl AnyAgent {
    pub async fn run_print(
        &self,
        prompt: &str,
        max_turns: usize,
        pure_stdout: bool,
    ) -> anyhow::Result<String> {
        match self {
            AnyAgent::OpenRouter(a) => runner::run_print(a, prompt, max_turns, pure_stdout).await,
            AnyAgent::OpenAI(a) => match a {
                OpenAiAgent::Responses(a) => {
                    runner::run_print(a, prompt, max_turns, pure_stdout).await
                }
                OpenAiAgent::Completions(a) => {
                    runner::run_print(a, prompt, max_turns, pure_stdout).await
                }
                OpenAiAgent::Codex(a) => runner::run_print(a, prompt, max_turns, pure_stdout).await,
            },
            AnyAgent::Anthropic(a) => runner::run_print(a, prompt, max_turns, pure_stdout).await,
            AnyAgent::Gemini(a) => runner::run_print(a, prompt, max_turns, pure_stdout).await,
            AnyAgent::Ollama(a) => runner::run_print(a, prompt, max_turns, pure_stdout).await,
            #[cfg(test)]
            AnyAgent::Test(_) => Ok(prompt.to_string()),
        }
    }

    #[cfg(feature = "subagents")]
    pub async fn run_subagent(
        &self,
        prompt: &str,
        max_turns: usize,
        event_tx: Option<&mpsc::Sender<AgentEvent>>,
    ) -> anyhow::Result<String> {
        match self {
            AnyAgent::OpenRouter(a) => runner::run_subagent(a, prompt, max_turns, event_tx).await,
            AnyAgent::OpenAI(a) => match a {
                OpenAiAgent::Responses(a) => {
                    runner::run_subagent(a, prompt, max_turns, event_tx).await
                }
                OpenAiAgent::Completions(a) => {
                    runner::run_subagent(a, prompt, max_turns, event_tx).await
                }
                OpenAiAgent::Codex(a) => runner::run_subagent(a, prompt, max_turns, event_tx).await,
            },
            AnyAgent::Anthropic(a) => runner::run_subagent(a, prompt, max_turns, event_tx).await,
            AnyAgent::Gemini(a) => runner::run_subagent(a, prompt, max_turns, event_tx).await,
            AnyAgent::Ollama(a) => runner::run_subagent(a, prompt, max_turns, event_tx).await,
            #[cfg(test)]
            AnyAgent::Test(_) => Ok(prompt.to_string()),
        }
    }

    pub fn spawn_runner(self, prompt: String, history: Vec<Message>) -> AgentRunner {
        match self {
            AnyAgent::OpenRouter(a) => runner::spawn_agent(a, prompt, history),
            AnyAgent::OpenAI(a) => match a {
                OpenAiAgent::Responses(a) => runner::spawn_agent(a, prompt, history),
                OpenAiAgent::Completions(a) => runner::spawn_agent(a, prompt, history),
                OpenAiAgent::Codex(a) => runner::spawn_agent(a, prompt, history),
            },
            AnyAgent::Anthropic(a) => runner::spawn_agent(a, prompt, history),
            AnyAgent::Gemini(a) => runner::spawn_agent(a, prompt, history),
            AnyAgent::Ollama(a) => runner::spawn_agent(a, prompt, history),
            #[cfg(test)]
            AnyAgent::Test(a) => a.spawn_runner(prompt),
        }
    }

    pub fn spawn_btw(
        self,
        prompt: String,
        history: Vec<Message>,
        event_tx: mpsc::Sender<crate::event::BtwEvent>,
        id: u32,
    ) -> crate::agent::runner::BtwRunner {
        match self {
            AnyAgent::OpenRouter(a) => runner::spawn_btw(a, prompt, history, event_tx, id),
            AnyAgent::OpenAI(a) => match a {
                OpenAiAgent::Responses(a) => runner::spawn_btw(a, prompt, history, event_tx, id),
                OpenAiAgent::Completions(a) => runner::spawn_btw(a, prompt, history, event_tx, id),
                OpenAiAgent::Codex(a) => runner::spawn_btw(a, prompt, history, event_tx, id),
            },
            AnyAgent::Anthropic(a) => runner::spawn_btw(a, prompt, history, event_tx, id),
            AnyAgent::Gemini(a) => runner::spawn_btw(a, prompt, history, event_tx, id),
            AnyAgent::Ollama(a) => runner::spawn_btw(a, prompt, history, event_tx, id),
            #[cfg(test)]
            AnyAgent::Test(_) => {
                let join = tokio::spawn(async move {
                    let _ = event_tx
                        .send(crate::event::BtwEvent::Done {
                            id,
                            response: CompactString::new(prompt),
                            input_tokens: 1,
                            output_tokens: 1,
                        })
                        .await;
                });
                crate::agent::runner::BtwRunner {
                    abort_handle: join.abort_handle(),
                }
            }
        }
    }
}

/// Expands a value that is exactly "${VAR}" to the environment variable's value;
/// any other format is returned as-is. Only whole-string `${VAR}` is supported
/// (the common, safe case) rather than arbitrary interpolation.
pub(crate) fn expand_env(value: &str) -> anyhow::Result<String> {
    if let Some(var) = value.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        std::env::var(var).map_err(|_| {
            anyhow::anyhow!(
                "Environment variable '{var}' (referenced in a custom provider header) is not set"
            )
        })
    } else {
        Ok(value.to_string())
    }
}

/// Builds a shared reqwest client, combining:
/// - `danger_accept_invalid_certs` (from #62; the TLS toggle shared by all providers)
/// - a custom provider's `headers` (values support `${ENV_VAR}` expansion) and `timeout_secs`
///
/// When the provider is not custom (`custom == None`) and TLS is not disabled,
/// the resulting client is equivalent to `reqwest::Client::default()`, so the
/// behavior of existing providers is unchanged.
pub(crate) fn build_http_client(
    provider_name: &str,
    danger_accept_invalid_certs: bool,
    custom: Option<&CustomProviderConfig>,
) -> anyhow::Result<reqwest::Client> {
    // Disable connection pooling. Local LLM servers (notably llama.cpp's
    // cpp-httplib) close idle keep-alive connections far faster than
    // reqwest's default 90s pool_idle_timeout, leaving stale half-closed
    // sockets in the pool. Reusing one of those manifests as
    // "error sending request" with no corresponding entry server-side
    // because no request actually reaches the server. TCP setup time is
    // negligible compared to inference time, so fresh connections per
    // request are a strict win for this workload.
    let mut builder = reqwest::Client::builder().pool_max_idle_per_host(0);

    if let Some(cfg) = custom {
        if !cfg.headers.is_empty() {
            let mut headers = HeaderMap::new();
            for (name, raw_value) in &cfg.headers {
                let value = expand_env(raw_value)?;
                let header_name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| anyhow::anyhow!("Invalid header name '{name}': {e}"))?;
                let header_value = HeaderValue::from_str(&value)
                    .map_err(|e| anyhow::anyhow!("Invalid value for header '{name}': {e}"))?;
                headers.insert(header_name, header_value);
            }
            builder = builder.default_headers(headers);
        }
        if let Some(secs) = cfg.timeout_secs {
            builder = builder.timeout(Duration::from_secs(secs));
        }
    }

    if danger_accept_invalid_certs {
        tracing::warn!(
            "TLS certificate verification DISABLED for provider '{}' \
             (danger_accept_invalid_certs = true). Connections are vulnerable to MITM.",
            provider_name
        );
        builder = builder.danger_accept_invalid_certs(true);
    }

    builder.build().map_err(Into::into)
}

/// Determines which API style the OpenAI family should use:
/// if `api_style` is set explicitly, honor it; otherwise default to Completions
/// when a base_url is present (i.e. a compatible gateway) and Responses when it
/// is absent (i.e. real api.openai.com).
pub(crate) fn resolve_api_style(
    base_url: Option<&str>,
    custom: Option<&CustomProviderConfig>,
) -> ApiStyle {
    custom.and_then(|c| c.api_style).unwrap_or({
        if base_url.is_some() {
            ApiStyle::Completions
        } else {
            ApiStyle::Responses
        }
    })
}

/// Builds an OpenAI-family client (Responses or Completions) using the
/// already-constructed shared http_client.
fn build_openai_client(
    key: &str,
    base_url: Option<&str>,
    custom: Option<&CustomProviderConfig>,
    http_client: reqwest::Client,
) -> anyhow::Result<OpenAiClient> {
    let style = resolve_api_style(base_url, custom);

    match style {
        ApiStyle::Responses => {
            let client = match base_url {
                Some(u) => openai::Client::builder()
                    .api_key(key)
                    .base_url(u)
                    .http_client(http_client)
                    .build()?,
                None => openai::Client::builder()
                    .api_key(key)
                    .http_client(http_client)
                    .build()?,
            };
            Ok(OpenAiClient::Responses(client))
        }
        ApiStyle::Completions => {
            let client = match base_url {
                Some(u) => openai::CompletionsClient::builder()
                    .api_key(key)
                    .base_url(u)
                    .http_client(http_client)
                    .build()?,
                None => openai::CompletionsClient::builder()
                    .api_key(key)
                    .http_client(http_client)
                    .build()?,
            };
            Ok(OpenAiClient::Completions(client))
        }
    }
}

pub fn create_client(
    provider_name: &str,
    api_key: Option<&str>,
    custom_providers: &HashMap<String, CustomProviderConfig>,
    config_api_keys: Option<&HashMap<String, String>>,
) -> anyhow::Result<AnyClient> {
    let config = resolve_provider_config(provider_name, custom_providers)?;
    let base_url = resolve_base_url(&config);

    if config.kind == ProviderKind::OpenAICodex {
        let custom = custom_providers.get(provider_name);
        let http_client =
            build_http_client(provider_name, config.danger_accept_invalid_certs, custom)?;
        return Ok(AnyClient::OpenAI(build_codex_client(http_client)?));
    }

    let resolver = AuthResolver::new(config.kind)
        .with_cli_key(api_key)
        .with_env_override(config.api_key_env.as_deref())
        .with_config_keys(config_api_keys)
        .with_custom_provider_name(Some(provider_name));
    let key = resolver.resolve()?;

    match config.kind {
        ProviderKind::OpenAI => {
            let custom = custom_providers.get(provider_name);
            let http_client =
                build_http_client(provider_name, config.danger_accept_invalid_certs, custom)?;
            Ok(AnyClient::OpenAI(build_openai_client(
                &key,
                base_url.as_deref(),
                custom,
                http_client,
            )?))
        }
        ProviderKind::Anthropic => build_anthropic_client(&key, base_url.as_deref()),
        ProviderKind::Gemini => build_gemini_client(&key, base_url.as_deref()),
        ProviderKind::Ollama => build_ollama_client(&key, base_url.as_deref()),
        ProviderKind::OpenRouter => build_openrouter_client(&key, base_url.as_deref()),
        ProviderKind::OpenAICodex => unreachable!("handled before static API-key resolution"),
    }
}

fn build_codex_client(http_client: reqwest::Client) -> anyhow::Result<OpenAiClient> {
    let client = openai::Client::builder()
        .api_key("dynamic-codex-auth")
        .base_url(OPENAI_CODEX_BASE_URL)
        .http_client(CodexHttpClient::new(http_client))
        .build()?;
    Ok(OpenAiClient::Codex(client))
}

macro_rules! build_provider_client {
    ($client_ty:ty, $variant:ident, $key_expr:expr, $base_url:expr) => {{
        let key = $key_expr;
        let builder = match $base_url {
            Some(u) => <$client_ty>::builder().api_key(key).base_url(u),
            None => <$client_ty>::builder().api_key(key),
        };
        Ok(AnyClient::$variant(builder.build()?))
    }};
}

fn build_anthropic_client(key: &str, base_url: Option<&str>) -> anyhow::Result<AnyClient> {
    build_provider_client!(anthropic::Client, Anthropic, key, base_url)
}

fn build_gemini_client(key: &str, base_url: Option<&str>) -> anyhow::Result<AnyClient> {
    build_provider_client!(gemini::Client, Gemini, key, base_url)
}

fn build_ollama_client(key: &str, base_url: Option<&str>) -> anyhow::Result<AnyClient> {
    build_provider_client!(
        ollama::Client,
        Ollama,
        ollama::OllamaApiKey::from(key),
        base_url
    )
}

fn build_openrouter_client(key: &str, base_url: Option<&str>) -> anyhow::Result<AnyClient> {
    // Expanded from `build_provider_client!` so we can chain OpenRouter's
    // builder-only app-identity calls: these set `X-OpenRouter-Title` /
    // `HTTP-Referer` / `X-OpenRouter-Categories` so zerostack's traffic is
    // attributed in OpenRouter's dashboards instead of showing up anonymously.
    let builder = match base_url {
        Some(u) => openrouter::Client::builder().api_key(key).base_url(u),
        None => openrouter::Client::builder().api_key(key),
    };
    let builder = builder
        .with_app_identity("zerostack", "https://github.com/gi-dellav/zerostack")
        .with_app_categories(&["cli-agent", "coding"]);
    Ok(AnyClient::OpenRouter(builder.build()?))
}

/// Builds an OpenAiModel (Responses / Completions) into the matching OpenAiAgent.
#[allow(clippy::too_many_arguments)]
async fn build_openai_agent(
    model: OpenAiModel,
    cli: &Cli,
    cfg: &Config,
    context: &ContextFiles,
    permission: Option<PermCheck>,
    ask_tx: Option<AskSender>,
    sandbox: Sandbox,
    reasoning_enabled: bool,
    temperature: Option<f64>,
    extra_body: Option<serde_json::Value>,
    #[cfg(feature = "mcp")] mcp_manager: Option<&McpClientManager>,
) -> OpenAiAgent {
    match model {
        OpenAiModel::Responses(m) => OpenAiAgent::Responses(
            builder::build_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                sandbox,
                reasoning_enabled,
                temperature,
                extra_body,
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await,
        ),
        OpenAiModel::Completions(m) => OpenAiAgent::Completions(
            builder::build_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                sandbox,
                reasoning_enabled,
                temperature,
                extra_body,
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await,
        ),
        OpenAiModel::Codex(m) => OpenAiAgent::Codex(
            builder::build_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                sandbox,
                reasoning_enabled,
                temperature,
                None,
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn build_agent(
    model: AnyModel,
    cli: &Cli,
    cfg: &Config,
    context: &ContextFiles,
    permission: Option<PermCheck>,
    ask_tx: Option<AskSender>,
    sandbox: Sandbox,
    reasoning_enabled: bool,
    temperature: Option<f64>,
    extra_body: Option<serde_json::Value>,
    #[cfg(feature = "mcp")] mcp_manager: Option<&McpClientManager>,
) -> AnyAgent {
    match model {
        AnyModel::OpenRouter(m, routing) => AnyAgent::OpenRouter(
            builder::build_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                sandbox.clone(),
                reasoning_enabled,
                temperature,
                merge_extra_body(routing, extra_body),
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await,
        ),
        AnyModel::OpenAI(m) => AnyAgent::OpenAI(
            build_openai_agent(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                sandbox.clone(),
                reasoning_enabled,
                temperature,
                extra_body,
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await,
        ),
        AnyModel::Anthropic(m) => AnyAgent::Anthropic(
            builder::build_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                sandbox.clone(),
                reasoning_enabled,
                temperature,
                extra_body,
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await,
        ),
        AnyModel::Gemini(m) => AnyAgent::Gemini(
            builder::build_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                sandbox.clone(),
                reasoning_enabled,
                temperature,
                extra_body,
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await,
        ),
        AnyModel::Ollama(m) => AnyAgent::Ollama(
            builder::build_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                sandbox.clone(),
                reasoning_enabled,
                temperature,
                extra_body,
                #[cfg(feature = "mcp")]
                mcp_manager,
            )
            .await,
        ),
        #[cfg(test)]
        AnyModel::Test(c) => AnyAgent::Test(TestAgent {
            prompts: c.prompts,
            sandbox,
        }),
    }
}

#[cfg(test)]
impl TestAgent {
    fn spawn_runner(self, prompt: String) -> AgentRunner {
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(32);
        let join = tokio::spawn(async move {
            self.prompts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(prompt.clone());
            if prompt == "sleep" {
                tokio::time::sleep(Duration::from_millis(50)).await;
                let _ = event_tx
                    .send(AgentEvent::ToolCall {
                        name: CompactString::new("bash"),
                        args: serde_json::json!({ "command": "bash -c 'sleep 500'" }),
                    })
                    .await;
                let _ = self.sandbox.output_command("bash -c 'sleep 500'").await;
            }
            let _ = event_tx
                .send(AgentEvent::Done {
                    response: CompactString::new(format!("received {prompt}")),
                    input_tokens: 1,
                    output_tokens: 1,
                })
                .await;
        });
        AgentRunner {
            event_rx,
            abort_handle: join.abort_handle(),
        }
    }
}

/// Builds the isolated, tool-less `/btw` agent for the active provider.
#[allow(clippy::too_many_arguments)]
pub fn build_btw_agent(
    model: AnyModel,
    cli: &Cli,
    cfg: &Config,
    context: &ContextFiles,
    permission: &Option<PermCheck>,
    ask_tx: &Option<AskSender>,
    reasoning_enabled: bool,
    temperature: Option<f64>,
    extra_body: Option<serde_json::Value>,
) -> AnyAgent {
    match model {
        AnyModel::OpenRouter(m, routing) => AnyAgent::OpenRouter(builder::build_btw_agent_inner(
            m,
            cli,
            cfg,
            context,
            permission,
            ask_tx,
            reasoning_enabled,
            temperature,
            merge_extra_body(routing, extra_body),
        )),
        AnyModel::OpenAI(m) => AnyAgent::OpenAI(match m {
            OpenAiModel::Responses(m) => OpenAiAgent::Responses(builder::build_btw_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                reasoning_enabled,
                temperature,
                extra_body,
            )),
            OpenAiModel::Completions(m) => {
                OpenAiAgent::Completions(builder::build_btw_agent_inner(
                    m,
                    cli,
                    cfg,
                    context,
                    permission,
                    ask_tx,
                    reasoning_enabled,
                    temperature,
                    extra_body,
                ))
            }
            OpenAiModel::Codex(m) => OpenAiAgent::Codex(builder::build_btw_agent_inner(
                m,
                cli,
                cfg,
                context,
                permission,
                ask_tx,
                reasoning_enabled,
                temperature,
                None,
            )),
        }),
        AnyModel::Anthropic(m) => AnyAgent::Anthropic(builder::build_btw_agent_inner(
            m,
            cli,
            cfg,
            context,
            permission,
            ask_tx,
            reasoning_enabled,
            temperature,
            extra_body,
        )),
        AnyModel::Gemini(m) => AnyAgent::Gemini(builder::build_btw_agent_inner(
            m,
            cli,
            cfg,
            context,
            permission,
            ask_tx,
            reasoning_enabled,
            temperature,
            extra_body,
        )),
        #[cfg(test)]
        AnyModel::Test(c) => AnyAgent::Test(TestAgent {
            prompts: c.prompts,
            sandbox: Sandbox::new(false, "bwrap"),
        }),
        AnyModel::Ollama(m) => AnyAgent::Ollama(builder::build_btw_agent_inner(
            m,
            cli,
            cfg,
            context,
            permission,
            ask_tx,
            reasoning_enabled,
            temperature,
            extra_body,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{ensure_codex_instructions, openrouter_anthropic_routing};
    use bytes::Bytes;
    use serde_json::json;

    #[test]
    fn pins_anthropic_namespaced_openrouter_models() {
        for id in [
            "anthropic/claude-sonnet-4.6",
            "anthropic/claude-opus-4.8",
            "anthropic/claude-3.5-haiku",
        ] {
            let extra = openrouter_anthropic_routing(id).expect("should pin {id}");
            assert_eq!(extra["provider"]["order"][0], "Anthropic");
            assert_eq!(extra["provider"]["allow_fallbacks"], true);
        }
    }

    #[test]
    fn pins_tilde_prefixed_latest_aliases() {
        // OpenRouter floating aliases carry a leading `~` that is part of the
        // real slug; they must still be pinned to the Anthropic route.
        for id in [
            "~anthropic/claude-sonnet-latest",
            "~anthropic/claude-opus-latest",
            "~anthropic/claude-haiku-latest",
        ] {
            assert!(
                openrouter_anthropic_routing(id).is_some(),
                "{id} should be pinned"
            );
        }
    }

    #[test]
    fn leaves_non_anthropic_openrouter_models_untouched() {
        for id in [
            "openai/gpt-4o",
            "deepseek/deepseek-chat",
            "google/gemini-2.5-pro",
            "openrouter/auto",
            // A non-Anthropic model that merely mentions claude in its path
            // is not in the anthropic namespace and must not be pinned.
            "somegateway/not-claude",
        ] {
            assert!(
                openrouter_anthropic_routing(id).is_none(),
                "{id} should not be pinned"
            );
        }
    }

    #[test]
    fn codex_request_body_promotes_system_message_to_instructions() {
        let body = json!({
            "model": "gpt-5.5",
            "instructions": null,
            "max_output_tokens": 128,
            "input": [
                {
                    "type": "message",
                    "role": "system",
                    "content": [{ "type": "input_text", "text": "Follow project rules." }]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "Hello" }]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "input_text", "text": "Hi there" }]
                }
            ]
        });

        let patched = ensure_codex_instructions(Bytes::from(body.to_string())).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&patched).unwrap();
        assert_eq!(value["instructions"], json!("Follow project rules."));
        assert_eq!(value["store"], json!(false));
        assert!(value.get("max_output_tokens").is_none());
        let input = value["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], json!("user"));
        assert_eq!(input[0]["content"][0]["type"], json!("input_text"));
        assert_eq!(input[1]["role"], json!("assistant"));
        assert_eq!(input[1]["content"][0]["type"], json!("output_text"));
    }
}
