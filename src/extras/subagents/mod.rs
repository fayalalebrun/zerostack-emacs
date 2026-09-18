use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;

use crate::event::AgentEvent;
use crate::provider::AnyClient;

pub(crate) mod builder;
pub(crate) mod prompt;
pub(crate) mod task_tool;
pub(crate) mod workspace;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelOption {
    pub name: String,
    pub provider: String,
    pub model: String,
}

pub(crate) fn resolve_model_options(
    config: &crate::config::Config,
    fallback_provider: &str,
    fallback_model: &str,
) -> Vec<ModelOption> {
    let names: Vec<String> = config
        .subagent_models
        .as_ref()
        .filter(|models| !models.is_empty())
        .map(|models| models.iter().map(ToString::to_string).collect())
        .or_else(|| {
            config
                .subagent_model
                .as_ref()
                .map(|model| vec![model.to_string()])
        })
        .unwrap_or_else(|| vec![fallback_model.to_string()]);
    let options = resolve_named_model_options(config, fallback_provider, &names);
    if options.is_empty() {
        vec![ModelOption {
            name: fallback_model.to_string(),
            provider: fallback_provider.to_string(),
            model: fallback_model.to_string(),
        }]
    } else {
        options
    }
}

pub(crate) fn resolve_named_model_options(
    config: &crate::config::Config,
    fallback_provider: &str,
    names: &[String],
) -> Vec<ModelOption> {
    let quick_models = crate::config::quick_models_map(config);
    let mut options = Vec::new();
    for name in names
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
    {
        if options
            .iter()
            .any(|option: &ModelOption| option.name == name)
        {
            continue;
        }
        let (provider, model) = quick_models
            .get(name)
            .map(|quick| (quick.provider.to_string(), quick.model.to_string()))
            .unwrap_or_else(|| (fallback_provider.to_string(), name.to_string()));
        options.push(ModelOption {
            name: name.to_string(),
            provider,
            model,
        });
    }
    options
}

pub(crate) struct SubagentConfig {
    pub client: AnyClient,
    pub provider_name: String,
    pub model_name: String,
    pub model_options: Vec<ModelOption>,
    runtime_model_override: bool,
    pub max_turns: usize,
    pub parent_session_id: String,
    pub config: crate::config::Config,
    pub agents: Option<String>,
    #[cfg(feature = "archmd")]
    pub architecture: Option<String>,
}

static CONFIG: Mutex<Option<SubagentConfig>> = Mutex::new(None);
static ENABLED: AtomicBool = AtomicBool::new(true);

static SUBAGENT_EVENT_TX: Mutex<Option<mpsc::Sender<AgentEvent>>> = Mutex::new(None);

pub(crate) fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub(crate) fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub(crate) fn set_subagent_event_tx(tx: mpsc::Sender<AgentEvent>) {
    let mut guard = SUBAGENT_EVENT_TX.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(tx);
}

pub(crate) fn clone_subagent_event_tx() -> Option<mpsc::Sender<AgentEvent>> {
    let guard = SUBAGENT_EVENT_TX.lock().unwrap_or_else(|e| e.into_inner());
    guard.clone()
}

pub(crate) fn with_config<F, R>(f: F) -> R
where
    F: FnOnce(&SubagentConfig) -> R,
{
    try_with_config(f).expect("subagents: SubagentConfig not initialized (call init() in main.rs)")
}

pub(crate) fn try_with_config<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&SubagentConfig) -> R,
{
    let guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().map(f)
}

pub fn init(
    client: AnyClient,
    provider_name: String,
    model_name: String,
    model_options: Vec<ModelOption>,
    max_turns: usize,
    parent_session_id: String,
    config: crate::config::Config,
    agents: Option<String>,
    #[cfg(feature = "archmd")] architecture: Option<String>,
) {
    let mut guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(SubagentConfig {
        client,
        provider_name,
        model_name,
        model_options,
        runtime_model_override: false,
        max_turns,
        parent_session_id,
        config,
        agents,
        #[cfg(feature = "archmd")]
        architecture,
    });
}

pub fn set_client_and_model(client: AnyClient, provider_name: String, model_name: String) {
    let mut guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cfg) = guard.as_mut() {
        cfg.client = client;
        cfg.provider_name = provider_name;
        cfg.model_name = model_name;
    }
}

pub fn set_model_options(model_options: Vec<ModelOption>) {
    let mut guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cfg) = guard.as_mut()
        && let Some(default) = model_options.first()
    {
        cfg.provider_name = default.provider.clone();
        cfg.model_name = default.model.clone();
        cfg.model_options = model_options;
    }
}

pub fn mark_runtime_model_override() {
    let mut guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cfg) = guard.as_mut() {
        cfg.runtime_model_override = true;
    }
}

pub fn follows_main_model() -> bool {
    let guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().is_none_or(|cfg| !cfg.runtime_model_override)
}

pub fn current_provider_model() -> Option<(String, String)> {
    let guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .map(|cfg| (cfg.provider_name.clone(), cfg.model_name.clone()))
}
