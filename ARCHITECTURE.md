# Architecture — zerostack v1.4.0-rc2

Minimal coding agent in Rust, optimized for memory footprint and performance.
Single crate, no workspace. All source under `src/`.

## Directory Layout

| Path | Responsibility |
|---|---|
| `src/main.rs` | Entry point, CLI dispatch, mode routing |
| `src/cli.rs` | `clap::Parser` CLI argument definition |
| `src/provider.rs` | LLM provider abstraction (type-erased: `AnyClient`, `AnyModel`, `AnyAgent` enums) |
| `src/auth.rs` | API key resolution (`AuthResolver`, `ProviderKind` enum) |
| `src/event.rs` | `AgentEvent` (streaming LLM output) and `UserEvent` (TUI input) enums |
| `src/agent/` | Agent lifecycle: `builder.rs` (rig Agent construction + tool injection), `runner.rs` (spawn, stream), `prompt.rs` (system prompts), `tools/` (core tool implementations, including feature-gated Veles search) |
| `src/session/` | Session state: `mod.rs` (messages, compactions, costs), `storage.rs` (JSON file I/O), `chat_history.rs` |
| `src/permission/` | Security: `checker.rs` (glob+regex rules, doom-loop detection), `ask.rs` (user prompt UI), `pattern.rs` |
| `src/ui/` | Custom TUI on crossterm (no ratatui): `mod.rs` (event loop), `terminal.rs` (raw mode guard), `renderer.rs` (line buffer + viewport), `input/` (text editor + pickers), `status.rs`, `markdown.rs`, `event_handler.rs`, `cmd_picker.rs` |
| `src/context/` | Context gathering: embedded themes (`themes.rs`), skills, AGENTS.md/ARCHITECTURE.md loading |
| `src/config/` | Configuration: `load.rs` (TOML/JSON from disk+env), `types.rs` (QuickModel, CustomProvider, Colors, EditSystem) |
| `src/extras/` | Feature-gated extensions: `loop/` (headless), `mcp/` (MCP client), `acp/` (ACP server), `memory/` (persistent memory), `subagents/` (separate-process task delegation + CoW workspaces), `git_worktree/`, `archmd/` |
| `src/sandbox.rs` | `bwrap`/`zerobox` command wrapping |
| `src/fs.rs` | Filesystem utilities |
| `src/pricing.rs` | Token pricing constants |

## Key Types & Relationships

- **`Config`** (`src/config/mod.rs:22`) — central deserialized config, drives all runtime behavior.
- **`Cli`** (`src/cli.rs:9`) — `clap::Parser` args, overrides `Config` fields.
- **`AnyClient` / `AnyModel` / `AnyAgent`** (`src/provider.rs`) — provider-routing enums wrapping concrete clients, Rig `DynModel<Completion>` handles, and non-generic `Agent` values. `AnyAgent` provides `run_print()` and `spawn_runner()`.
- **`AgentRunner`** (`src/agent/runner.rs:12`) — holds `mpsc::Receiver<AgentEvent>`, spawned via `spawn_agent()`.
- **`AgentEvent`** (`src/event.rs:4`) — `Token`, `Reasoning`, `ToolCall`, `ToolResult`, `SubagentToolCall`, `Error`, `Done`.
- **`UserEvent`** (`src/event.rs:27`) — `Key`, `ScrollUp/Down`, `Resize`, `Paste`, `MouseDown/Drag/Up`.
- **`Session`** (`src/session/mod.rs:39`) — serializable state: messages, compactions, costs, permission allowlist, model/provider info.
- **`PermissionChecker`** (`src/permission/checker.rs:29`) — dual-layer (glob + regex) rules, doom-loop detection, `SecurityMode` dispatch.
- **`TerminalGuard`** (`src/ui/terminal.rs:10`) — RAII for raw mode, alt screen, mouse capture.
- **`Renderer`** (`src/ui/renderer.rs:21`) — line-buffered viewport, markdown rendering, scroll/selection.
- **`InputEditor`** (`src/ui/input/mod.rs:22`) — text buffer, cursor, history, kill-ring, picker integration.
- **`ContextFiles`** (`src/context/mod.rs:56`) — loaded repository instructions, skills, themes, and architecture docs.

## Control Flow

```
CLI parse (main.rs:88) → config load → context load → session load
  │
  ├── --print-config → print and exit
  ├── --acp → extras::acp::serve()
  ├── --print → single agent.run_print() response
  ├── --loop → run_headless_loop() iterative mode
  └── (default) → ui::run_interactive()
```

### Interactive TUI Event Loop (`src/ui/mod.rs`)

Single `tokio::select!` with 4 branches (line ~310):
1. **`UserEvent` from `user_rx`** — keyboard/mouse/resize/paste from background event thread (polls crossterm every 50ms)
2. **`AgentEvent` from `agent_rx`** — streaming LLM tokens, tool calls, errors
3. **Permission `AskRequest` from `ask_rx`** — user must approve/reject tool calls
4. **Periodic refresh** (100ms) — spinner animation when agent is running

Key dispatch: `InputEditor::handle_key()` → `Some(text)` triggers `spawn_agent()` → stream events via `handle_agent_event()` which writes to `Renderer` and appends to `Session`.

## Data Flow

```
User input → InputEditor (buffer) → spawn_agent(prompt + history)
  │
  ▼
Agent (rig) → DynModel<Completion> (LLM API)
  │
  ▼ streaming
AgentEvent stream (Token, ToolCall, ToolResult, ...)
  │
  ├── handle_agent_event() → Renderer (viewport buffer) → crossterm draw commands
  ├── ToolCall → PermissionChecker.check() → {Allowed, Ask, Denied}
  │     ├── Ask → permission_handler (user approves/rejects via UI)
  │     └── Allowed → tool execution (bash/read/write/edit/grep/etc.)
  └── Done → Session.append() → session::storage::save_session()
```

Session is serialized to JSON files in `$XDG_DATA_HOME/zerostack/sessions/`. Chat history appended to `$XDG_DATA_HOME/zerostack/chat_history.jsonl`.

## Design Decisions

1. **Custom TUI over crossterm (no ratatui)** — keeps binary size minimal; project has its own line buffer, markdown renderer, scroll/selection. No widget tree overhead.
2. **Provider routing over erased Rig models** — enums preserve provider-specific routing and configuration; Rig 0.44 erases completion models and agent types. Contextual `Tool` and `DynamicTool` implementations retain zerostack permission checks. Streaming uses `Item<StreamEvent>` and `AgentHook` captures provider-native assistant turns for persisted reasoning replay. (`src/provider.rs`, `src/agent/runner.rs`, `src/session/mod.rs`)
3. **Permission: dual-layer (glob + regex) rules** — glob for fast path, regex for complex patterns. Doom-loop detection tracks repeated identical tool calls. (`src/permission/checker.rs:29`)
4. **Session compaction** — when token count approaches context window, old messages are summarized and dropped, preserving a summary prefix. (`src/session/mod.rs:24`)
5. **Feature-gated extras** — `loop`, `mcp`, `acp`, `memory`, `subagents`, `git-worktree`, `archmd`, and `veles` are compile-time features. Extras don't bloat builds that disable them.
6. **Single-threaded tokio by default** — `#[tokio::main(flavor = "current_thread")]` unless `multithread` feature enabled. Keeps resource usage low for a CLI tool.
7. **Process-isolated subagents** — `task` starts a one-shot zerostack child with its own persisted session and a read-only Emacs socket for real-time attachment. Write tasks snapshot the current working state into a persistent CoW Git worktree or copied directory; parent sessions store links and access rules for child transcripts/workspaces. (`src/extras/subagents/task_tool.rs`, `workspace.rs`, `src/extras/emacs.rs`)

8. **Worktree-shared Veles index** — the feature-gated embedded Veles tool stores indexes under the zerostack cache directory, keyed by the canonical Git common directory. Calls from all worktrees serialize through a per-repository file lock and refresh the shared index from the active worktree. (`src/agent/tools/veles.rs`)

9. **Shared persistent-session launcher** — `src/extras/session_cli.rs` owns systemd user-service creation, environment inheritance, private retained logs, session identity resolution, startup readiness, and timeout cleanup. Both `zerostack session start` and the Emacs client's `--emacs-launch` entry point call it. Emacs only connects to the returned socket; daemons run outside the caller's cgroup. (`src/main.rs`, `emacs/zerostack.el`)

10. **Compact Emacs tool activity** — Rust owns call grouping, structured outcomes, and append-only structured detail logs. Live summaries are batched at 100 ms; Emacs updates a cached row by message index, with no transcript scan or per-call overlays. On demand, details show one cached row per call with status, duration, and output/diff links; full arguments and IDs open separately. Failures, permissions, and task/goal progress stay visible. Reconnects reconstruct compact history from canonical session messages. (`src/extras/emacs.rs`, `emacs/zerostack.el`)

11. **One core system prompt** — `src/agent/prompt.rs` defines task scope, quality, verification, safety, and communication. `build_preamble()` adds tool guidance, repository context, skills, added files, memory, and `SUFFIX.md`; selectable prompt modes and prompt chaining are not supported. Security modes remain enforced by the permission checker.

## Dependencies

| Crate | Use |
|---|---|
| `rig 0.44` | Agent hooks, contextual tools, streaming, erased completion models, provider clients |
| `clap 4` | Derive-based CLI argument parsing (`src/cli.rs:9`) |
| `crossterm 0.29` | Terminal raw mode, color, cursor, mouse, paste events — TUI foundation |
| `tokio 1` | Async runtime (current_thread default), channels (`mpsc`), process, fs |
| `serde + serde_json + toml` | Config (TOML/JSON), session serialization (JSON) |
| `chrono`, `uuid` | Session timestamps and IDs |
| `pulldown-cmark 0.13` | Markdown → styled lines for TUI rendering |
| `ignore 0.4` | `.gitignore`-aware file traversal (`find_files` tool) |
| `regex 1` | Permission pattern matching |
| `reqwest 0.13` | HTTP client (provider API calls via rig) |
| `tracing + tracing-subscriber` | Structured logging (`RUST_LOG` env var) |
| `mimalloc` | Global allocator (size + speed) |
| `compact_str`, `smallvec` | Heap-efficient small-string/small-vector types |
| `veles-core 0.6` | Optional embedded hybrid code search and persistent repository indexes |

Optional (`mcp` feature): `rmcp 1.8` (MCP client with child-process + HTTP transport). Optional (`acp` feature): `agent-client-protocol 0.12`.

## Entry Points

- **`main()`** (`src/main.rs:83`) — all modes dispatch from here
- **`--print`** / `-p` — `agent.run_print()` → single reply, then exit (`main.rs:243`)
- **`--loop`** — `run_headless_loop()` → iterative prompt/validate loop (`main.rs:262`)
- **`--acp`** — `extras::acp::serve()` → ACP server mode (`main.rs:210`)
- **Default (no flags)** — `ui::run_interactive()` → full TUI (`main.rs:295`)
- **`--resume`** / `--continue` / `--session <id>` — loads prior session before entering TUI/print
