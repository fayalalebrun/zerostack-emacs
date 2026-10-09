---
name: zerostack-board
description: Create isolated workspaces and launch or prompt persistent zerostack sessions from the CLI without Emacs.
---

# Zerostack workspace and session automation

Use the bash tool to invoke the installed `zerostack` binary. These commands do
not require Emacs. Do not start an interactive TUI from a tool call.

1. Inspect existing projects, worktrees, saved sessions and live sockets:
   Start with `zerostack board list --summary` to avoid large session records.
   Narrow with `--repo /absolute/repo`, `--path /absolute/workspace`,
   `--session UUID-OR-PREFIX`, and/or `--alive`; filters combine with AND.
   Summaries give counts and workspace metadata, not session IDs or sockets.
   Drop `--summary` for targeted details. Default output is unchanged JSON;
   projects contain worktrees/sessions, and non-Git directories are under
   `loose_workspaces`. Paths match exact board workspace paths, not descendants.
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
   Use `zerostack workspace wait --job FULL-JOB-UUID --timeout 600` rather than
   repeated polling. It reports phase changes and ten-second timing heartbeats
   to stderr, then final JSON to stdout. Only `ready` exits successfully; failure,
   interruption, and timeout return nonzero. Timeout does not cancel setup:
   wait on the same job again. Status/wait include total and per-phase timings
   for new jobs. Read complete combined hook stdout/stderr with
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
   Alternatively, add `--start-session --prompt "Concrete task"` to
   `workspace create` to wait for successful setup and then start the session.
   Optional `--provider`, `--model`, and `--timeout` require `--start-session`.
   This prints one JSON object containing `workspace` and `session` on success.
   Failed/timed-out setup never starts a session. After a setup timeout, wait on
   the saved job and use `session start`, not another create command. Session
   startup failure retains the workspace; initial-prompt failure may leave a live
   worker, with metadata in the error. Inspect before retrying.
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
