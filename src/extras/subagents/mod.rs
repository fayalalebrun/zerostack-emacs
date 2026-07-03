use std::sync::Mutex;

use tokio::sync::mpsc;

use crate::event::AgentEvent;
use crate::provider::AnyClient;

pub(crate) mod builder;
pub(crate) mod prompt;
pub(crate) mod task_tool;

pub(crate) struct SubagentConfig {
    pub client: AnyClient,
    pub provider_name: String,
    pub model_name: String,
    pub max_turns: usize,
    pub config: crate::config::Config,
    pub agents: Option<String>,
    #[cfg(feature = "archmd")]
    pub architecture: Option<String>,
}

static CONFIG: Mutex<Option<SubagentConfig>> = Mutex::new(None);

static SUBAGENT_EVENT_TX: Mutex<Option<mpsc::Sender<AgentEvent>>> = Mutex::new(None);

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
    max_turns: usize,
    config: crate::config::Config,
    agents: Option<String>,
    #[cfg(feature = "archmd")] architecture: Option<String>,
) {
    let mut guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(SubagentConfig {
        client,
        provider_name,
        model_name,
        max_turns,
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

pub fn set_model_name(model_name: String) {
    let mut guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cfg) = guard.as_mut() {
        cfg.model_name = model_name;
    }
}

pub fn current_provider_model() -> Option<(String, String)> {
    let guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .map(|cfg| (cfg.provider_name.clone(), cfg.model_name.clone()))
}
