use std::collections::HashSet;

use anyhow::Context as _;
use compact_str::CompactString;

use crate::auth::ProviderKind;
use crate::config::Config;

const BUILTIN_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "openai-codex",
    "gemini",
    "openrouter",
    "ollama",
];

pub fn builtin_providers() -> &'static [&'static str] {
    BUILTIN_PROVIDERS
}

pub fn provider_names(cfg: &Config) -> Vec<String> {
    let mut names: Vec<String> = builtin_providers()
        .iter()
        .map(|p| (*p).to_string())
        .collect();
    let mut seen: HashSet<String> = names.iter().cloned().collect();
    let mut custom: Vec<String> = cfg.custom_providers_map().into_keys().collect();
    custom.sort();
    for name in custom {
        if seen.insert(name.clone()) {
            names.push(name);
        }
    }
    names
}

pub fn default_provider_name(cfg: &Config) -> String {
    cfg.provider
        .as_deref()
        .map(canonical_provider_name)
        .unwrap_or_else(|| {
            crate::config::quick_models_map(cfg)
                .get("deepseek-v4-pro")
                .map(|q| q.provider.to_string())
                .unwrap_or_else(|| "openrouter".to_string())
        })
}

pub fn validate_provider(cfg: &Config, provider: &str) -> anyhow::Result<()> {
    if crate::provider::parse_provider(provider).is_some()
        || cfg.custom_providers_map().contains_key(provider)
    {
        Ok(())
    } else {
        anyhow::bail!(
            "unknown provider '{}'. Supported providers: {}",
            provider,
            provider_names(cfg).join(", "),
        )
    }
}

pub fn model_ids_for_provider(provider: &str) -> Vec<String> {
    crate::models_catalog::catalog_entries(provider)
        .unwrap_or(&[])
        .iter()
        .filter(|entry| crate::provider::is_agent_model(entry))
        .map(|entry| entry.id.clone())
        .collect()
}

pub fn set_default_provider(cfg: &mut Config, provider: &str) -> anyhow::Result<(String, String)> {
    validate_provider(cfg, provider)?;
    let provider = canonical_provider_name(provider);
    let model = crate::provider::default_model_for_provider(&provider, cfg)
        .map(|(model, _)| model)
        .or_else(|| cfg.model.as_ref().map(ToString::to_string))
        .with_context(|| {
            format!(
                "provider '{}' has no default model; configure a model for the provider first",
                provider,
            )
        })?;

    cfg.provider = Some(CompactString::new(&provider));
    cfg.model = Some(CompactString::new(&model));
    Ok((provider, model))
}

pub fn set_default_model(cfg: &mut Config, model: &str) -> anyhow::Result<(String, String)> {
    if model.trim().is_empty() {
        anyhow::bail!("model cannot be empty");
    }
    let provider = default_provider_name(cfg);
    validate_provider(cfg, &provider)?;
    cfg.provider = Some(CompactString::new(&provider));
    cfg.model = Some(CompactString::new(model));
    Ok((provider, model.to_string()))
}

#[cfg(feature = "subagents")]
pub fn set_subagent_provider(cfg: &mut Config, provider: &str) -> anyhow::Result<(String, String)> {
    validate_provider(cfg, provider)?;
    let provider = canonical_provider_name(provider);
    let model = crate::provider::default_model_for_provider(&provider, cfg)
        .map(|(model, _)| model)
        .or_else(|| cfg.subagent_model.as_ref().map(ToString::to_string))
        .or_else(|| cfg.model.as_ref().map(ToString::to_string))
        .with_context(|| {
            format!(
                "provider '{}' has no default model; configure a subagent model first",
                provider,
            )
        })?;

    cfg.subagent_provider = Some(CompactString::new(&provider));
    cfg.subagent_model = Some(CompactString::new(&model));
    Ok((provider, model))
}

#[cfg(feature = "subagents")]
pub fn set_subagent_model(cfg: &mut Config, model: &str) -> anyhow::Result<(String, String)> {
    if model.trim().is_empty() {
        anyhow::bail!("subagent model cannot be empty");
    }
    let provider = cfg
        .subagent_provider
        .as_deref()
        .map(canonical_provider_name)
        .unwrap_or_else(|| default_provider_name(cfg));
    validate_provider(cfg, &provider)?;
    cfg.subagent_provider = Some(CompactString::new(&provider));
    cfg.subagent_model = Some(CompactString::new(model));
    Ok((provider, model.to_string()))
}

pub fn canonical_provider_name(provider: &str) -> String {
    match crate::provider::parse_provider(provider) {
        Some(ProviderKind::OpenRouter) => "openrouter".to_string(),
        Some(ProviderKind::OpenAI) => "openai".to_string(),
        Some(ProviderKind::OpenAICodex) => "openai-codex".to_string(),
        Some(ProviderKind::Anthropic) => "anthropic".to_string(),
        Some(ProviderKind::Gemini) => "gemini".to_string(),
        Some(ProviderKind::Ollama) => "ollama".to_string(),
        None => provider.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use compact_str::CompactString;

    use super::*;
    use crate::config::{ApiStyle, CustomProviderConfig};

    #[test]
    fn provider_names_include_builtins_then_sorted_custom() {
        let mut cfg = Config::default();
        cfg.custom_providers = Some(HashMap::from([
            (
                "z-local".to_string(),
                custom_provider_config(Some("local-model")),
            ),
            (
                "a-gateway".to_string(),
                custom_provider_config(Some("gateway-model")),
            ),
        ]));

        let names = provider_names(&cfg);
        let builtin: Vec<&str> = names[..builtin_providers().len()]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(builtin, builtin_providers());
        assert!(names.ends_with(&["a-gateway".to_string(), "z-local".to_string()]));
    }

    #[test]
    fn set_provider_canonicalizes_alias_and_resets_model() {
        let mut cfg = Config {
            provider: Some(CompactString::new("openrouter")),
            model: Some(CompactString::new("deepseek/deepseek-v4-pro")),
            ..Config::default()
        };

        let (provider, model) = set_default_provider(&mut cfg, "codex").unwrap();

        assert_eq!(provider, "openai-codex");
        assert_eq!(model, "gpt-5.5");
        assert_eq!(cfg.provider.as_deref(), Some("openai-codex"));
        assert_eq!(cfg.model.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn set_provider_uses_custom_configured_model() {
        let mut cfg = Config::default();
        cfg.custom_providers = Some(HashMap::from([(
            "local".to_string(),
            custom_provider_config(Some("llama-local")),
        )]));

        let (provider, model) = set_default_provider(&mut cfg, "local").unwrap();

        assert_eq!(provider, "local");
        assert_eq!(model, "llama-local");
    }

    #[test]
    fn model_ids_for_codex_use_openai_catalog() {
        let ids = model_ids_for_provider("openai-codex");
        assert!(ids.iter().any(|id| id == "gpt-5.5"));
    }

    #[test]
    fn set_model_persists_current_default_provider() {
        let mut cfg = Config {
            provider: Some(CompactString::new("google")),
            ..Config::default()
        };

        let (provider, model) = set_default_model(&mut cfg, "gemini-2.5-pro").unwrap();

        assert_eq!(provider, "gemini");
        assert_eq!(model, "gemini-2.5-pro");
        assert_eq!(cfg.provider.as_deref(), Some("gemini"));
        assert_eq!(cfg.model.as_deref(), Some("gemini-2.5-pro"));
    }

    #[cfg(feature = "subagents")]
    #[test]
    fn set_subagent_provider_and_model_persist_subagent_defaults() {
        let mut cfg = Config {
            model: Some(CompactString::new("main-model")),
            ..Config::default()
        };

        let (provider, model) = set_subagent_provider(&mut cfg, "openrouter").unwrap();

        assert_eq!(provider, "openrouter");
        assert!(!model.is_empty());
        assert_eq!(cfg.subagent_provider.as_deref(), Some("openrouter"));
        assert_eq!(cfg.subagent_model.as_deref(), Some(model.as_str()));

        let (provider, model) =
            set_subagent_model(&mut cfg, "deepseek/deepseek-chat-v3.1").unwrap();

        assert_eq!(provider, "openrouter");
        assert_eq!(model, "deepseek/deepseek-chat-v3.1");
        assert_eq!(cfg.subagent_provider.as_deref(), Some("openrouter"));
        assert_eq!(
            cfg.subagent_model.as_deref(),
            Some("deepseek/deepseek-chat-v3.1")
        );
    }

    fn custom_provider_config(model: Option<&str>) -> CustomProviderConfig {
        CustomProviderConfig {
            provider_type: CompactString::new("openai"),
            base_url: "http://localhost:11434/v1".to_string(),
            api_key_env: None,
            danger_accept_invalid_certs: None,
            api_style: Some(ApiStyle::Completions),
            headers: HashMap::new(),
            timeout_secs: None,
            model: model.map(CompactString::new),
        }
    }
}
