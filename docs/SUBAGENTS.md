# Subagents

## Overview

The `task` tool starts a separate zerostack process with its own persisted
session transcript. Read subagents operate in the current workspace. Write
subagents operate in a persistent Git worktree (or a copied directory outside a
Git repository) containing the parent's tracked, staged, unstaged, and untracked
edits at spawn time. File copies use CoW reflinks when the filesystem supports
them.

The tool returns when the child agent loop or child process exits. The requested
`timeout` is a soft deadline: when it expires, active work is interrupted and
exactly one tool-free turn summarizes completed work, existing verification,
and anything remaining. That final turn has no hard timeout. Aborting the parent
turn still terminates the child process group. Results include the response,
child session ID, workspace path,
transcript path, and live socket path. While the child is running, Emacs
attaches to that same process and receives streamed updates.

## Feature Gate

Subagents are **opt-in** via the `subagents` Cargo feature:

```toml
# Cargo.toml
[features]
default = ["loop", "git-worktree", "mcp", "subagents"]
```

## The `task` Tool

The main agent has a tool called `task` with this request:

```json
{
  "task": "implement and test the auth fix",
  "access": "write",
  "timeout": 600,
  "model": "deepseek-v4-pro",
  "reasoning": "high"
}
```

`task`, `access`, and `timeout` (soft-deadline seconds) are required. `model`
accepts either a model ID or a configured quick-model name. `reasoning` accepts
the same effort values as `--reasoning-effort`.

## Access modes

- `read`: runs in the current workspace under read-only permissions.
- `write`: snapshots the current state into a persistent workspace, runs with
  normal write tools there, and sandboxes shell writes to that workspace when a
  supported sandbox backend is available.

The parent `task` call still passes through normal permission checks. Child
sessions cannot recursively spawn subagents. The parent receives read/list
access to read workspaces, read/write/edit/list access to isolated write
workspaces, and read access to the session transcript.

## Configuration

| Config field           | Type      | Default             | Description                           |
|------------------------|-----------|---------------------|---------------------------------------|
| `task_max_turns`       | `usize`   | `20`                | Max agent turns per subagent          |
| `task_enabled`         | `bool`    | `true`              | Global default for all subagent use   |
| `subagent_models`      | `string[]` | current main model | Permitted model IDs or quick-model aliases; first is the default |
| `subagent_model`       | `string`  | none | Legacy single-model form, used only when `subagent_models` is absent |
| `subagent_provider`    | `string`  | same as main | Provider for raw model IDs in the list |

### Model permissions and resolution

`subagent_models` is the authoritative allowlist exposed to the parent agent as
the `SpawnRequest.model` enum. A requested model outside this list is rejected
before a process is spawned. The first entry is used when `model` is omitted.
Quick-model aliases resolve to their configured provider and model; raw model IDs
use `subagent_provider` (or the main provider). The legacy `subagent_model`
setting supplies a one-item list when `subagent_models` is absent.

When the subagent uses a different provider than the main agent, a separate
API client is created at startup. The subagent client is independent from the
main agent's client and can be switched at runtime.

Set the default for new sessions with `zerostack config set-subagents true|false`.
Each session stores its own value; `/subagents on|off` changes that session only.
Disabling subagents removes the `task` tool and prevents automatic goal evaluators.

Example `opencode.json`:

```json
{
  "task_max_turns": 20,
  "task_enabled": true,
  "subagent_models": ["deepseek-v4-flash", "deepseek-v4-pro", "gpt-5.5"],
  "subagent_provider": "openrouter"
}
```

## Goal Evaluation

`goal_update` tracks a single active implementation goal separately from ordinary
todos. Agents should create one only when the user explicitly asks for a
tracked goal/active goal, or when updating an existing active goal. A
`completed` goal is a verified completion claim and must include:

- `evidence`: concrete proof such as commands run with relevant output, files
  changed, or explicit user confirmation.
When evidence is present, `goal_update` automatically runs a narrow evaluator
subagent. The tool stores `evaluator_status` and `evaluator_summary` from that
report. If evidence is missing, or the evaluator verdict is not `PASS`,
`goal_update` rejects the completed status and leaves the active goal unchanged.
`blocked` is accepted only with concrete evidence of an external dependency,
missing user input, or permission denial, and it is evaluated by the same
independent subagent path. `cancelled` is accepted only with concrete evidence of
a user-requested scope change.

This intentionally works with a smaller subagent model: the evaluator receives
only one goal and the claimed evidence. It must return `PASS`, `FAIL`, or
`INSUFFICIENT` with citations; it does not infer broad intent from the full
transcript.

## Slash Commands

| Command                            | Description                                |
|------------------------------------|--------------------------------------------|
| `/subagents [on|off]`              | Show or set subagent use for this session  |
| `/model-subagent [name]`           | Show or switch to one permitted runtime model |
| `/models-subagent [name]`          | List quick models or switch to a one-item runtime allowlist |

- **`/model-subagent`** with no arguments shows the current subagent provider
  and model. With a model name, it switches the subagent to that model (using
  the same provider).
- **`/models-subagent`** with no arguments lists quick models. With a quick
  model name, it switches the subagent to that quick model's provider + model.
  If the quick model uses a different provider, a new API client is created.

These legacy TUI commands reduce the runtime allowlist to the selected model.
In Emacs, Hydra `S` opens a multi-select **subagent models** picker and updates
the runtime allowlist. The board's `models […]` control persists the default
allowlist for future sessions.

## Architecture

```
parent agent ──task──> workspace snapshot ──spawn──> zerostack -p
     │                                                    │
     ├── live child-session link ─── Emacs socket ────────┤
     ├── workspace access                                 ├── agent loop
     └── transcript access <──────── response/exit ───────┤ session JSON
```

Key files:

| File                                         | Role                                  |
|----------------------------------------------|---------------------------------------|
| `src/extras/subagents/mod.rs`                | Module root, static config            |
| `src/extras/subagents/task_tool.rs`          | `TaskTool` implementation             |
| `src/extras/subagents/workspace.rs`          | CoW workspace snapshots               |
| `src/extras/emacs.rs` (`serve_subagent`)     | Live child agent loop, socket, and transcript |
| `src/agent/builder.rs`                       | Wires `TaskTool` into the parent agent |
| `src/extras/emacs_board.rs`                  | Parent/child session board metadata   |
| `emacs/zerostack.el`                         | Nested sessions and session links     |

The older in-process read agent builder remains for narrow goal evaluation.
`task` subagents always use the separate-process path.

## Emacs

Child sessions are persisted with their parent session ID and access mode. The
Emacs board nests them under the parent session in a list that starts collapsed;
it does not expose isolated write workspaces as separate board rows. Opening a
child session sets its workspace as `default-directory`, so normal `find-file`
access stays scoped to that session. Deleting the child session also removes its
Git worktree or copied workspace. During a task call, Emacs renders a clickable
child-session link that lists the model, provider, thinking level, access mode,
and soft finalization deadline. Opening
running connects to the child process's Unix socket and streams its real-time
updates. The attachment is read-only so it cannot start a competing turn or
mutate the child session. On completion, the child's model response is attached
to the parent transcript as a clickable text artifact. Reopening the parent uses
the same artifact renderer, while reopening the child session link uses its
persisted transcript.
