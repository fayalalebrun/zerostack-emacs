//! Static, embedded model catalog.
//!
//! Model ids change rarely between releases, so instead of hitting each
//! provider's `/models` endpoint at startup (slow — OpenRouter alone returns
//! hundreds of entries and used to block the first frame), we bake a snapshot
//! into the binary. The picker is seeded from this synchronously, with zero
//! network. The live listing is still available on demand via `/models refresh`
//! (see [`crate::ui::slash`]) and for providers not baked here (custom gateways,
//! ollama).
//!
//! The data lives in `data/models.json`, keyed by *zerostack* provider name
//! (so `gemini`, not models.dev's `google`). Refresh it with
//! `scripts/gen-models-catalog.sh`.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::provider::ModelEntry;

const CATALOG_JSON: &str = include_str!("../data/models.json");

#[derive(serde::Deserialize)]
struct RawModel {
    id: String,
    name: String,
    context: Option<u32>,
    input: Option<u32>,
}

static CATALOG: LazyLock<HashMap<String, Vec<ModelEntry>>> = LazyLock::new(|| {
    let raw: HashMap<String, Vec<RawModel>> = serde_json::from_str(CATALOG_JSON)
        .expect("embedded data/models.json is malformed — run scripts/gen-models-catalog.sh");
    raw.into_iter()
        .map(|(provider, models)| {
            let entries = models
                .into_iter()
                .map(|m| ModelEntry {
                    id: m.id,
                    display: m.name,
                    context_length: m
                        .context
                        .map(|context| m.input.map_or(context, |input| context.min(input))),
                    kind: None,
                })
                .collect();
            (provider, entries)
        })
        .collect()
});

/// Baked model entries for a provider, or `None` when the provider is not in the
/// catalog (custom gateways, ollama — those resolve live).
pub fn catalog_entries(provider: &str) -> Option<&'static [ModelEntry]> {
    let provider = match provider {
        "openai-codex" | "codex" => "openai",
        other => other,
    };
    CATALOG.get(provider).map(|v| v.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(provider: &str) -> Vec<String> {
        catalog_entries(provider)
            .unwrap_or(&[])
            .iter()
            .map(|m| m.id.clone())
            .collect()
    }

    #[test]
    fn catalog_parses_and_has_expected_providers() {
        for p in [
            "anthropic",
            "deepseek",
            "openai",
            "gemini",
            "opencode-go",
            "openrouter",
        ] {
            assert!(
                !ids(p).is_empty(),
                "missing or empty baked catalog for: {p}"
            );
        }
    }

    #[test]
    fn deepseek_includes_default_models() {
        // The direct DeepSeek defaults must be discoverable offline so the picker
        // is useful on a fresh, network-blocked start.
        let ids = ids("deepseek");
        assert!(ids.contains(&"deepseek-v4-flash".to_string()));
        assert!(ids.contains(&"deepseek-v4-pro".to_string()));
    }

    #[test]
    fn opencode_go_matches_the_live_catalog_snapshot() {
        let ids = ids("opencode-go");
        let expected = [
            "minimax-m3",
            "minimax-m2.7",
            "minimax-m2.5",
            "kimi-k3",
            "kimi-k2.7-code",
            "kimi-k2.6",
            "longcat-2.0",
            "kimi-k2.5",
            "glm-5.2",
            "glm-5.3-flash",
            "glm-5.3",
            "glm-5.1",
            "glm-5",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek-v4-flash-vision-exp",
            "qwen3.7-max",
            "qwen3.8-max",
            "qwen3.8-flash",
            "qwen3.7-plus",
            "qwen3.6-plus",
            "qwen3.5-plus",
            "mimo-v2-pro",
            "mimo-v2-omni",
            "mimo-v2.5-pro",
            "mimo-v2.5",
            "hy4-preview",
            "hy3",
            "hy3-preview",
            "gpt-5.6-luna",
            "grok-4.5",
            "grok-4.6",
            "muse-spark-1.3-contributor",
            "muse-spark-1.2-contributor",
            "omen-alpha",
        ];
        assert_eq!(ids.len(), expected.len());
        for model in expected {
            assert!(ids.contains(&model.to_string()), "missing model: {model}");
        }
    }

    #[test]
    fn openrouter_includes_deepseek_provider_models() {
        // The OpenRouter-prefixed DeepSeek models remain available when using
        // OpenRouter explicitly.
        // offline so the picker is useful on a fresh, network-blocked start.
        assert!(
            ids("openrouter").contains(&"deepseek/deepseek-v4-pro".to_string()),
            "default model missing from baked openrouter catalog"
        );
    }

    #[test]
    fn unbaked_provider_has_no_catalog() {
        // ollama resolves live (local), so it is intentionally not baked.
        assert!(catalog_entries("ollama").is_none());
    }

    #[test]
    fn openai_codex_uses_openai_catalog() {
        let openai = catalog_entries("openai").unwrap();
        let codex = catalog_entries("openai-codex").unwrap();
        assert_eq!(openai.len(), codex.len());
        assert!(codex.iter().any(|m| m.id == "gpt-5.5"));
        assert!(codex.iter().any(|m| m.id == "gpt-6-astra"));
    }
}
