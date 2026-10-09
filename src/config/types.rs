use std::collections::HashMap;

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickModelConfig {
    pub provider: CompactString,
    pub model: CompactString,
    #[serde(default)]
    pub input_token_cost: f64,
    #[serde(default)]
    pub output_token_cost: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<u64>,
    /// Per-model temperature override (0.0–2.0). Takes precedence over the
    /// global `temperature` setting but is overridden by `--temperature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<serde_json::Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "reasoning-effort"
    )]
    pub reasoning_effort: Option<CompactString>,
}

/// Status-bar statusline layout. Up to 3 lines, each an ordered list of segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusLineConfig {
    #[serde(default)]
    pub lines: Vec<StatusLineLine>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusLineLine {
    #[serde(default)]
    pub segments: Vec<StatusLineSegment>,
}

/// Icon for a statusline item: `true` uses the item's built-in glyph, or a
/// string sets a custom one (a named icon like `branch`, or a literal glyph).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IconSpec {
    Auto(bool),
    Custom(CompactString),
}

/// One statusline piece. `item` names the element (see `docs/CONFIG.md`).
/// `color`/`bg` are named colors or `#rrggbb`. `text` is the literal for the
/// `separator` item. `left`/`right` are powerline cap glyphs drawn before/after
/// the item. `icon` shows a glyph before the value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusLineSegment {
    pub item: CompactString,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<IconSpec>,
    /// Force a numeric item (`tokens_input`, `tokens_output`, `cost`) to show
    /// even when its value is zero (normally hidden).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiStyle {
    Responses,
    Completions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomProviderConfig {
    pub provider_type: CompactString,
    pub base_url: String,
    pub api_key_env: Option<CompactString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub danger_accept_invalid_certs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_style: Option<ApiStyle>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<CompactString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditSystem {
    #[default]
    Similarity,
    Hashedit,
}

impl std::fmt::Display for EditSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditSystem::Similarity => write!(f, "similarity"),
            EditSystem::Hashedit => write!(f, "hashedit"),
        }
    }
}

impl std::str::FromStr for EditSystem {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "similarity" => Ok(EditSystem::Similarity),
            "hashedit" => Ok(EditSystem::Hashedit),
            _ => Err(format!(
                "unknown edit system '{}' (valid: similarity, hashedit)",
                s
            )),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorsConfig {
    pub chat_background: Option<CompactString>,
    pub input_background: Option<CompactString>,
    pub status_background: Option<CompactString>,
}

#[cfg(feature = "advisor")]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvisorConfig {
    pub enabled: bool,
    pub model: Option<CompactString>,
    pub max_uses: Option<usize>,
    pub human_handoff: bool,
    pub advisor_kilobytes_limit: u32,
}

#[cfg(feature = "advisor")]
impl Default for AdvisorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: Some(CompactString::new("deepseek-v4-pro")),
            max_uses: Some(3),
            human_handoff: true,
            advisor_kilobytes_limit: 256,
        }
    }
}
