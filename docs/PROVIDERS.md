# Providers

zerostack supports built-in providers and custom provider definitions for
OpenAI-compatible endpoints.

## Built-in Providers

| Provider | Config name | Default env var for API key |
| --- | --- | --- |
| OpenRouter | `openrouter` | `OPENROUTER_API_KEY` |
| OpenAI | `openai` | `OPENAI_API_KEY` |
| OpenAI Codex | `openai-codex` / `codex` | ChatGPT subscription auth |
| OpenCode Go | `opencode-go` | `OPENCODE_GO_API_KEY` |
| DeepSeek | `deepseek` | `DEEPSEEK_API_KEY` |
| Anthropic | `anthropic` | `ANTHROPIC_API_KEY` |
| Gemini | `gemini` / `google` | `GEMINI_API_KEY` |
| Ollama | `ollama` | (no key required) |

Select a provider via the config file, the `--provider` CLI flag, or the
`ZS_PROVIDER` environment variable:

```
zerostack --provider anthropic
```

The model is set with `--model` or `ZS_MODEL`:

```
zerostack --provider openai --model gpt-4o
```

## OpenCode Go

Subscribe to OpenCode Go, copy its API key, then set `OPENCODE_GO_API_KEY` or
store it locally:

```bash
zerostack auth set-key opencode-go <key>
zerostack --provider opencode-go --model kimi-k2.7-code
```

zerostack sends `User-Agent: zerostack/<version>` and `x-opencode-session` on
all Go API requests, including model refreshes. The session header uses the
conversation's persisted ID; auxiliary clients without a session ID get a
UUID retained across requests and client clones.

OpenCode Go serves some models through OpenAI Chat Completions, OpenAI
Responses, or Anthropic Messages. zerostack selects the documented API style
for each bundled model automatically. The embedded 35-model snapshot combines
the published endpoint table with the live catalog and is used at startup; run
`/models refresh` in the TUI to replace it with the current
live `/models` catalog. The snapshot is also available with:

```bash
zerostack config models opencode-go
```

`reasoning-effort` is passed only when the selected Go model declares that
level. For example, GLM-5.3-Flash supports `low`, `high`, and `max`; GPT 5.6
Luna supports `none`, `low`, `medium`, `high`, `xhigh`, and `max` (not `minimal`);
and models without an effort control do not receive an unsupported request
parameter. Go subagents honor per-model `reasoning-effort` overrides; Messages
subagents also receive the configured `max_tokens` limit.

### Context limits

All 35 bundled Go models include context metadata from
[Models.dev](https://models.dev/api.json). The legacy `hy3-preview` entry uses
Tencent TokenHub's 256,000-token preview limit because Go no longer publishes
metadata for that alias. These are provider-specific limits, not limits borrowed
from similarly named OpenAI, Codex, or OpenRouter models.

| Model | Effective context budget (tokens) |
| --- | ---: |
| GLM 5.2 / 5.3 / 5.3 Flash | 1,000,000 |
| GLM 5 / 5.1 | 202,752 |
| Kimi K2.5 / K2.6 / K2.7 Code | 262,144 |
| Kimi K3 | 1,048,576 |
| GPT 5.6 Luna | 922,000 |
| Qwen 3.6 Plus / 3.7 / 3.8 | 1,000,000 |
| Qwen 3.5 Plus | 262,144 |
| DeepSeek V4 Pro / Flash / Flash Vision Exp | 1,000,000 |
| MiniMax M3 | 1,000,000 |
| MiniMax M2.5 / M2.7 | 204,800 |
| Hy3 / Hy3 Preview / Hy4 Preview | 192,000 / 256,000 / 1,024,000 |
| Grok 4.5 / 4.6 / Omen Alpha | 500,000 |
| Muse Spark 1.2 / 1.3 Contributor | 1,048,576 |
| MiMo V2.5 / V2.5 Pro / V2 Pro / V2 Omni | 1,000,000 / 1,048,576 / 1,048,576 / 262,144 |
| LongCat 2.0 | 1,000,000 |

The catalog stores total `context` and, when published, a separate `input`
ceiling. zerostack uses the smaller value for budgeting: Luna's total window is
1,050,000 but its input ceiling is 922,000; Hy3's are 256,000 and 192,000.
The configured response reserve is still subtracted before auto-compaction.
An explicit `context_window` configuration overrides catalog lookup.

`/models refresh` retains these limits for known models; unknown IDs still use
the 128,000-token fallback unless configured explicitly. Restart and resume
an existing Go session to replace its old fallback with the catalog limit.
`scripts/gen-models-catalog.sh` refreshes limits for the curated Go IDs without
removing legacy aliases or unrelated provider entries; test it with
`bash scripts/test-models-catalog.sh`.

## Custom Providers

Custom providers let you point zerostack at any OpenAI-compatible API (vLLM,
LiteLLM, Ollama, local models, enterprise gateways, etc.). Define them under
the `custom_providers` key in the config file:

```json
{
  "custom_providers": {
    "local-vllm": {
      "provider_type": "openai",
      "base_url": "http://localhost:8000/v1",
      "api_key_env": "VLLM_API_KEY",
      "model": "gemma4"
    },
    "company-gateway": {
      "provider_type": "openai",
      "base_url": "https://gateway.example.com/v1",
      "model": "glm"
    }
  }
}
```

| Field                         | Type    | Description                                                                                                                                                                   |
| ----------------------------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `provider_type`               | string  | Must be one of the built-in provider config names shown above.                                                                                                                |
| `base_url`                    | string  | The API base URL.                                                                                                                                                             |
| `api_key_env`                 | string  | Optional. Name of an environment variable holding the API key. Falls back to the provider-kind default if not set.                                                            |
| `api_style`                   | string  | Optional. For OpenAI-based providers: `"responses"` (Responses API, default when no `base_url` is set) or `"completions"` (Chat Completions, default when `base_url` is set). |
| `headers`                     | object  | Optional. HTTP headers to include in every request. Values support `${ENV_VAR}` expansion.                                                                                    |
| `danger_accept_invalid_certs` | boolean | Optional. Disables TLS certificate verification (MITM risk — use with care).                                                                                                  |
| `timeout_secs`                | integer | Optional. Overrides the default HTTP timeout.                                                                                                                                 |
| `model`                       | string  | Optional. Default model name for this provider. Used when no model is specified via `--model` or `ZS_MODEL`.                                                                  |

### Header variable expansion

Header values can reference environment variables with `${VAR}` syntax:

```json
{
  "custom_providers": {
    "company-gateway": {
      "provider_type": "openai",
      "base_url": "https://gateway.example.com/v1",
      "headers": {
        "cf-access-client-id": "${CF_ACCESS_CLIENT_ID}",
        "cf-access-client-secret": "${CF_ACCESS_CLIENT_SECRET}"
      }
    }
  }
}
```

## API Key Resolution

The API key is resolved in this priority order:

1. **CLI flag** `--api-key` (visible in process listings — use with care)
2. **Environment variable** — either the custom one from `api_key_env`, or the
   default env var for the provider kind
3. **Config file** `api_keys` map — keyed by provider slug or custom provider name
4. **Ollama** — returns an empty string (no key required)

### Config-level API keys

```json
{
  "api_keys": {
    "openai": "sk-...",
    "anthropic": "sk-ant-..."
  }
}
```

## OpenAI API Styles

The OpenAI provider supports two API transports:

- **Responses API** (`/responses`) — the default for OpenAI's own API. Required
  for GPT-5-series models that reject `max_tokens` on Chat Completions.
- **Chat Completions API** (`/chat/completions`) — the default when a custom
  `base_url` is set, since most OpenAI-compatible gateways implement only this
  endpoint.

Override with `api_style: "responses"` or `api_style: "completions"` on a
custom provider, or set `api_style` on the built-in OpenAI provider to force a
specific transport.

## Prompt caching

zerostack enables prompt caching automatically where the underlying rig provider supports it. The behavior depends on which provider backs the model you choose.

### Automatic — no zerostack action

These providers cache server-side without any markers in the request:

- **OpenAI** — automatic above ~1024 tokens, 50% discount on cached input.
- **Google Gemini 2.5+** — implicit caching, 75% discount.
- **DeepSeek** — automatic, persistent across days, ~90% discount.
- **xAI / Grok** — automatic, 75% discount; benefits from setting an `x-grok-conv-id` header which zerostack does not currently send.
- **Moonshot, Groq (Kimi K2)** — automatic.

For these providers, zerostack passes through to rig without additional configuration.

### Explicitly enabled by zerostack

These providers require `cache_control` markers; zerostack adds them via rig's `.with_prompt_caching()`:

- **Anthropic (direct API)** — marks system prompt, the final tool definition, and the last message. All three breakpoints contribute to cumulative savings as the conversation grows.
- **Claude via OpenRouter** — marks the system prompt only. Anthropic's caching is prefix-based, so the tools array in front of the system block is also captured. For `anthropic/*` model IDs, zerostack also pins `provider.order = ["Anthropic"]` with `allow_fallbacks: true`, because Bedrock and Vertex AI silently drop `cache_control` markers.

### Empirical impact

Measured on Sonnet 4.6, second turn of a tool-heavy session (grep + read across the zerostack repo, ~6k-token system prompt including AGENTS.md and ARCHITECTURE.md):

| Configuration               | turn 2 cost | reduction |
| --------------------------- | ----------: | --------: |
| Baseline (no caching)       |      $0.186 |         — |
| Anthropic + caching         |      $0.024 |      -87% |
| OpenRouter Claude + caching |      $0.026 |      -86% |

Projected monthly cost at 50 such turns per working day: $204 (baseline) → $26 (Anthropic direct) or $29 (OpenRouter Claude). The two cached paths are within ~$3/month of each other.

### Known limitation: OpenRouter does not mark the last message

As of rig 0.38, OpenRouter's `apply_prompt_caching` marks the system message only. The Anthropic provider also marks the last message, which means accumulated tool results from earlier turns continue to be cached as the conversation grows; OpenRouter does not mark this position.

On tool-heavy workloads this manifests as ~2,676 tokens running at full input rate on OpenRouter vs ~4 tokens on Anthropic direct, a gap of ~8% per turn. The bulk of savings comes from caching the system prompt and tools — both paths capture that.

This is an upstream rig limitation, not zerostack-specific.

**Recommendation:** if you happen to have both API keys, Anthropic direct is marginally cheaper. If you don't, OpenRouter Claude with caching captures most of the savings.

## Provider retries

Interactive provider requests retry transient failures with exponential backoff:
2, 4, 8, 16, then 30 seconds between subsequent attempts. Retryable failures
include HTTP 408, 429, and all 5xx responses, plus provider overload,
unavailability, rate-limit, timeout, exhausted-capacity, and transient connection
errors. Authentication, invalid requests, context overflow, billing and account
usage limits, unsupported models, and content-policy failures are not retried.

When a failure follows partial output, zerostack preserves that assistant output,
appends a synthetic `Go` user message, and continues from the resulting history.
This keeps the model aware of text and completed tool interactions from the failed
request instead of replaying the original request blindly.

## CLI Flags

| Flag                | Env var       | Description                         |
| ------------------- | ------------- | ----------------------------------- |
| `--provider`        | `ZS_PROVIDER` | Provider name                       |
| `--model`           | `ZS_MODEL`    | Model name                          |
| `--quick-model`     | —             | Use a named quick model from config |
| `--api-key`         | —             | API key (visible in `ps`)           |
| `--max-tokens`      | —             | Maximum response tokens             |
| `--temperature`     | —             | Model temperature (0.0–2.0)         |
| `--max-agent-turns` | —             | Maximum agent turns per response    |
