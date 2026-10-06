---
name: zerostack-board
description: Create isolated workspaces and launch or prompt persistent zerostack sessions from the CLI without Emacs.
---

# Zerostack workspace and session automation

Use the bash tool to invoke the installed `zerostack` binary. These commands do
not require Emacs. Do not start an interactive TUI from a tool call.

1. Inspect existing projects, worktrees, saved sessions and live sockets:
   `zerostack board list`. Output is JSON; projects contain worktrees and
   sessions, with non-Git directories under `loose_workspaces`.
2. Create an isolated Git workspace:
   `zerostack workspace create --repo /absolute/repo --branch task-name --path /absolute/new-workspace --description "Task purpose"`.
   On Unix, output is JSON with `job`, `path`, `branch`, `status`, and `log`.
   Creation and hooks run as a detached job; success here means submitted,
   not hydrated. Save the job ID before doing anything else.
   Requires the `git-worktree` feature.
   The base defaults to `origin/HEAD`; select `--base HEAD` explicitly when
   appropriate. Creation does not fetch remote refs. Paths must not exist.
   Repository-local prepare and hydrate hooks run sequentially in the worker. Hooks are
   executable repository policy: only use repositories trusted by the user.
   A failed hydrate leaves the created workspace in place; inspect the error
   rather than blindly retrying or deleting it.
   Poll `zerostack workspace status --job FULL-JOB-UUID` using short tool calls
   with a pause between polls. Read complete combined hook stdout/stderr with
   `zerostack workspace logs --job FULL-JOB-UUID`, or tail the returned log path.
   Status progresses through `queued`, `preparing`, `creating`, `hydrating` to
   `ready` or `failed`. `interrupted` means the worker died without completion;
   the workspace may be partial. Never treat an existing directory or successful
   create submission as completion. If submission times out, inspect saved jobs
   under `$ZS_DATA_DIR/workspace-jobs` (default `~/.local/share/zerostack/workspace-jobs`)
   before retrying. Do not delete partial workspaces or restart jobs blindly.
3. Only after workspace status is `ready`, start a persistent background worker:
   `zerostack session start --path /absolute/new-workspace --prompt "Concrete task and acceptance criteria"`.
   Optionally select `--provider NAME --model ID`. Output is JSON containing
   the full session UUID, PID, socket and log path, after protocol readiness.
   Omitting `--prompt` creates an idle worker. The worker persists independently
   of this CLI command and can be attached through the Emacs board later.
4. Send another turn to an idle worker:
   `zerostack session send --session FULL-UUID --prompt "Follow-up task"`.
   Success means the prompt was accepted, not that the task finished. Busy
   workers reject additional prompts. Never blindly retry a timed-out send:
   it may already have been accepted.
5. Inspect `zerostack board list` and the returned log path for metadata and
   startup diagnostics. The log is not the assistant transcript. Live workers
   need a socket client such as Emacs for streaming transcript and permissions.

Normal permission checks remain enabled. A worker may wait for user approval;
do not bypass permissions to make delegation unattended. Provide a clear task,
workspace boundaries, and validation instructions. Do not create commits, push,
remove workspaces, or stop workers unless the user explicitly requests it.
