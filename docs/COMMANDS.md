# Slash Commands

All slash commands are available from the TUI input prompt.

## Emacs compact activity

Consecutive tool calls, including reads, searches, commands, edits, and MCP
calls, share one compact activity row. Assistant prose and inline task/goal
progress remain in conversational order. `[activity]` opens a separate, styled
buffer with one row per call: status, a short call summary, duration, and
output/diff links. `[details]` opens full arguments and the call ID separately;
raw JSON, IDs, blank separators, and duplicate result headings stay out of the
activity list. Press `g` to read new records and update only affected call rows;
no background refresh hooks or per-call overlays are installed. Existing logs
use the same compact layout when refreshed with the updated client.

Rust batches activity-row updates at 100 ms intervals; Emacs replaces only the
cached row through a direct marker lookup, without scanning or rebuilding the
transcript. Reconnecting and loading saved history also use compact rows.
“Complete” means calls returned, not that their outputs were successful:
failures are counted and remain visible in chat, permission controls stay inline,
and unfinished calls are marked stopped. Older results without structured
outcomes are labeled unknown rather than inferred from output text.

Reload the updated Emacs client and restart idle daemons to enable this view;
running sessions are not restarted automatically.

## Session

| Command | Description |
| ------- | ----------- |
| `/clear` | Clear the current session (all messages, tokens, compactions). |
| `/undo` | Remove the last exchange (user message + assistant response). |
| `/retry` | Load the last user message into the input editor for editing. |
| `/quit` | Exit zerostack. |
| `/sessions` | List recent saved sessions (up to 20). |
| `/sessions <id-prefix>` | Load a session by its ID prefix. |
| `/sessions delete <id-prefix>` | Delete a session by its ID prefix. |
| `/timing` | Show individual command and text-generation block wait times, slowest first. |
| `/history` | Show global chat history (last 10 entries across sessions). |
| `/fork [message-index]` | Fork the current conversation into a new session before a selected user message, or before `message-index`. |

Resuming an interrupted session preserves completed tool calls and results.
Native tool calls without a recorded result are replayed as an interruption
notice, not executed automatically: a command may already have had side effects.
Empty assistant turns are omitted from provider history; the saved transcript
is left unchanged.

## Provider & Model

| Command | Description |
| ------- | ----------- |
| `/provider` | Show the current provider. |
| `/provider <name>` | Switch to a different provider. |
| `/model` | Show the current model. |
| `/model <name>` | Switch to a different model. |
| `/models` | List all quick models defined in config. |
| `/models <name>` | Switch to a named quick model. |
| `/models-add <name> <provider> <model>` | Save a new quick model to the config file. |

## Provider Configuration & Authentication

These are top-level CLI commands, not slash commands:

| Command | Description |
| ------- | ----------- |
| `zerostack config providers` | List built-in and custom provider names. |
| `zerostack config models [provider]` | List baked model IDs for a provider, or for the current default provider when omitted. Custom/uncataloged providers may return no rows so clients can allow manual model entry. |
| `zerostack config set-provider <provider>` | Persist the default provider in config and reset the default model to that provider's configured/default model. |
| `zerostack config set-model <model>` | Persist the default model for the current default provider. |
| `zerostack auth login codex` | Log in to ChatGPT Codex subscription auth with the browser/redirect flow. |
| `zerostack auth login codex --device` | Log in with the device-code flow. |
| `zerostack auth status` | Show stored provider auth state. |
| `zerostack auth logout codex` | Remove stored Codex credentials. |

Codex credentials are stored in `auth.json` under the zerostack config directory
with private file permissions. Use them with `--provider openai-codex` and a Codex
model such as `gpt-5.1-codex`. Provider requests reload and refresh the shared
credentials while a long-lived Emacs/TUI session is running; the next request will
use the updated file.

## Context Files

| Command | Description |
| ------- | ----------- |
| `/add` | List files currently added to context (with sizes). |
| `/add <path>` | Add a file to the agent's context (absolute or relative path). |
| `/drop <path>` | Remove a file from the agent's context. |
| `/drop-all` | Remove all added files from the agent's context. |

Files added with `/add` are included alongside the conversation in each request,
useful for giving the agent reference documentation or code without cluttering
the chat directly.

## Initialization

| Command | Description |
| ------- | ----------- |
| `/init` | Create an AGENTS.md file for the current project by delegating to the agent. |
| `/init force` | Overwrite the existing AGENTS.md if one already exists. |

Requires a `code` prompt to be configured (run `/regen-prompts` to restore
built-in prompts, or create a custom `code.md` prompt).

## Security

| Command | Description |
| ------- | ----------- |
| `/mode` | Show the current security mode. |
| `/mode standard` | Allow path tools within CWD, ask for external paths. Config rules apply. |
| `/mode restrictive` | Ask for every operation. Config rules skipped. |
| `/mode readonly` | Allow reads only; deny writes, edits, bash, and everything else. |
| `/mode guarded` | Allow reads; ask for writes, edits, bash, and everything else. Config rules apply. |
| `/mode yolo` | Allow everything; ask for destructive bash commands. Config rules apply. |

Prompts can set the security mode automatically via `%%mode=<mode>` on
the first line. When a prompt with `%%mode=last_user_mode` is activated,
the mode reverts to whatever was last set explicitly by `/mode` or
startup config. See Prompts & Themes below.

## Prompts & Themes

| Command | Description |
| ------- | ----------- |
| `/prompt` | List available prompts. |
| `/prompt <name>` | Activate a named prompt. Also applies `%%mode=` from the prompt file if present (see below). |
| `/prompt default` | Clear the active prompt. |

Prompts may include a `%%mode=<mode>` directive on the **first line** to
automatically switch the security mode when activated. Valid modes:
`standard`, `restrictive`, `readonly`, `guarded`, `yolo`. Use
`%%mode=last_user_mode` to restore the mode the user last set via `/mode`
or startup config. The directive line is stripped from the prompt content
before it reaches the agent.

Example `ask.md`:
```markdown
%%mode=readonly

## Read-Only Mode

You are in read-only mode. Only read files and explore.
```
| `/theme` | List available themes. |
| `/theme <name>` | Activate a named theme. |
| `/theme default` | Clear the active theme (use config colors). |
| `/regen-prompts` | Restore built-in prompts to the prompts directory. |
| `/regen-themes` | Restore built-in themes to the themes directory. |

## Conversation

| Command | Description |
| ------- | ----------- |
| `/compress [instructions]` | Compress conversation history to free context window space. |
| `/compact` | Alias for `/compress`. |
| `/editsys` | Show the current edit system mode (similarity or hashedit). |
| `/editsys similarity` | Use SEARCH/REPLACE with fuzzy matching for edits (default). |
| `/editsys hashedit` | Use CRC-32 tag-based edits (token-efficient, CAS-guarded). |
| `/btw <message>` | Ask a quick side question in parallel, without touching the main conversation. It forks the current context (including the main agent's in-flight turn, if any), answers using read-only tools (read/grep/find_files/list_dir, no writes or bash), and prints the answer inline. Works even while the main agent is running. Nothing is written to history; its token cost is shown separately as `btw:$…`. Ctrl-C cancels an in-flight `/btw` without disturbing the main agent. |
| `/reasoning` | Toggle LLM reasoning on/off (requires model support). |
| `/thinking` | Alias for `/reasoning`. |
| `/review [msg]` | Run a one-shot code review. Activates the `review` prompt in readonly mode, submits a review message, and restores the previous prompt afterward. Without a message, auto-generates one based on session and worktree context. |

Before each successful compaction, zerostack archives the complete pre-compaction
session under `<data-dir>/sessions/compacted/<session-id>/`. Session JSON also
stores each provider call's token usage and `duration_ms`; tool results retain
their existing `duration_ms`, allowing provider-wait and tool time analysis.
| `/toggle` | Show available toggleable features. |
| `/toggle todo [on\|off]` | Enable or disable todo-list tools. |

## Memory (feature-gated)

Requires building with `--features memory`.

| Command | Description |
| ------- | ----------- |
| `/memory` | Show memory status (MEMORY.md, scratchpad, daily log). |
| `/memory status` | Same as `/memory` (explicit status check). |
| `/memory search <query>` | Search all memory files with case-insensitive keyword matching. |
| `/memory read long_term` | Read the global MEMORY.md file. |
| `/memory read scratchpad` | Read the project scratchpad (open checklist items). |
| `/memory read daily [date]` | Read a daily log (defaults to today; use YYYY-MM-DD for past). |
| `/memory read note <name>` | Read a named note. |
| `/memory write long_term <content>` | Append to the global MEMORY.md. |
| `/memory write scratchpad <content>` | Append to the project scratchpad. |
| `/memory write daily <content>` | Append to today's daily log. |
| `/memory write note:<name> <content>` | Append to a named note. |
| `/memory editor` | Open MEMORY.md in your system `$EDITOR`. |
| `/memory clear scratchpad` | Clear all scratchpad items. |
| `/memory clear daily` | Clear all of today's entries. |

Long-term memory (MEMORY.md) and open scratchpad items are automatically injected
into every request. Daily logs (today + yesterday) are also included. Notes and
older daily logs are accessible via `/memory read` and `memory_search`.

## MCP (feature-gated)

| Command | Description |
| ------- | ----------- |
| `/mcp` | List connected MCP servers and their tool counts. |
| `/mcp <server>` | List tools of a specific MCP server. |
| `/mcp login <server>` | Run the OAuth 2.0 login flow for a URL server, then reconnect it. |
| `/mcp logout <server>` | Remove a server's stored OAuth token. |

## Advisor (feature-gated)

| Command | Description |
| ------- | ----------- |
| `/advisor` | Show current advisor status (enabled, mode, model, max uses). |
| `/advisor on` | Enable the advisor tool. |
| `/advisor off` | Disable the advisor tool. |
| `/advisor handoff` | Toggle human handoff mode on. |
| `/advisor handoff on` | Enable human handoff mode (route calls to the user). |
| `/advisor handoff off` | Disable human handoff mode (use advisor model). |
| `/advisor model <name>` | Change the advisor model. |
| `/advisor max-uses <n>` | Set max advisor calls per request (0 = unlimited). |
| `/advisor context-limit <n>` | Set max kilobytes of conversation context sent to advisor. |

## Worktree (feature-gated)

| Command | Description |
| ------- | ----------- |
| `/worktree <name>` | Create a git worktree on a new branch and `cd` into it. |
| `/wt-merge [branch]` | Merge the worktree branch back into the target branch. |
| `/wt-exit` | Exit the worktree and return to the main repo. |

Worktree creation sources
`$(git rev-parse --git-common-dir)/zerostack/workspace` when present. Optional
`prepare` and `hydrate` shell functions run before and after creation. They
receive `ZEROSTACK_WORKSPACE_PHASE`, `ZEROSTACK_WORKTREE_NAME`,
`ZEROSTACK_WORKTREE_PATH`, `ZEROSTACK_REPO_ROOT`, and
`ZEROSTACK_GIT_COMMON_DIR`. Emacs board creation runs `prepare` synchronously
before Git creation and launches `hydrate` asynchronously in a visible shell
command buffer after creation.

## Loop (feature-gated)

| Command | Description |
| ------- | ----------- |
| `/loop [prompt]` | Start the iterative coding loop. |
| `/loop stop` | Stop the active loop. |
| `/loop status` | Show current loop status. |

## Shell Commands

Prefix a message with `!` to run it as a shell command instead of sending it to
the agent. The command's output is captured and stored in the session history as
an Assistant message. Works in both TUI and `--print` mode.

| Example | Description |
| ------- | ----------- |
| `!ls -la` | List files in the current directory. |
| `!git status` | Check git status without involving the agent. |
| `!cargo test` | Run tests and capture the output. |
| `!` | Empty command shows an error. |

If you want to run a command and then discuss the output with the agent, just
type `!<command>` first (it stores the output as an Assistant message), then
follow up with a normal message asking the agent about it.

## Native Emacs Protocol

`M-x zerostack-restart-idle-sessions`, or `R` on the zerostack board, restarts
all live idle sessions, including detached sessions without open chat buffers.
It fetches a fresh daemon-published snapshot and skips running, permission-waiting,
and starting sessions. Legacy daemons without activity metadata count as idle.
Detached sessions restart through an undisplayed temporary controller that closes
once startup succeeds; failures are reported without stopping other restarts.

`M-x zerostack-timing` opens a separate buffer with individual commands and
text-generation blocks for the current session, sorted from slowest to fastest.

`zerostack --emacs` runs one headless zerostack session as a Unix socket server
and registers it under `$XDG_RUNTIME_DIR/zerostack/sessions/<session-id>/` (or
`$ZS_RUNTIME_DIR/sessions/<session-id>/` when set). The session directory
contains `sock`, `pid`, `meta.json`, and ephemeral per-turn artifacts under
`artifacts/`. Use `zerostack --emacs-list` to list live registered sessions and
clean up stale entries.

The socket protocol is one escaped S-expression per line. Core client commands:

| Command | Description |
| ------- | ----------- |
| `(hello :protocol 1 :cols 100)` | Negotiate protocol and set render width. |
| `(attach :cols 100)` | Receive a full rendered session snapshot. |
| `(prompt :request 1 :text "...")` | Start one agent turn. |
| `(set-view :cols 120)` | Change markdown render width for later updates. |
| `(provider :request 2 :provider "openai-codex")` | Switch this live Emacs session to another provider and reset the session model to that provider's configured/default model. Rejected while a prompt or loop is running. |
| `(model :request 2 :model "gpt-5.5")` | Switch this live Emacs session to another model for the current provider. Rejected while a prompt or loop is running. |
| `(file-add :request 2 :path "/path/to/file")` | Queue a file for the next prompt. Text files become extra context; with feature `multimodal`, recognized images/audio/PDFs become media attachments. |
| `(file-list :request 2)` | Return queued context files and media attachments as easy-parse item plists. |
| `(file-drop :request 2 :path "/path/to/file")` | Remove one queued file/media attachment by path. `:index N` can be used with indexes from `file-list`. |
| `(file-drop-all :request 2)` | Remove all queued context files and media attachments. |
| `(compact :request 2 :instructions "...")` | Compress session history using optional instructions, then emit a fresh `session-render`. |
| `(loop-start :request 3 :prompt "..." :max 5 :run "cargo test")` | Start the iterative loop using optional max iterations, plan file, and validation command. |
| `(loop-stop :request 4)` | Stop an active Emacs loop and abort the active loop turn when one is running. |
| `(loop-status :request 5)` | Return current loop status fields such as `:active`, `:iteration`, `:label`, `:max`, `:plan`, and `:prompt`. |
| `(abort)` | Abort the active turn. |
| `(permission-answer :request 9 :decision allow-once)` | Answer a permission prompt. Decisions are `allow-once`, `allow-always`, or `deny`; `allow-always` may include `:pattern "..."`. |
| `(list-sessions :limit 50)` | Return live native Emacs sessions. |
| `(dismiss-attention :session "...")` | Remove a session from the board Needs attention section. Defaults to the current session when `:session` is omitted. |

Compaction completion events include the post-compaction `:tokens` and
`:context-window` values. End-of-turn automatic compaction immediately continues
the same agent turn from the compacted session instead of entering the idle state.

When a loop is active, one-off `(prompt ...)` commands are rejected until the loop
is stopped. Loop events are broadcast as ordinary protocol events:
`loop-started` when the loop is accepted, `loop-iteration` before each agent
iteration, and `loop-stopped` with `:reason stopped` or `:reason max`. Each loop
iteration still emits normal render/tool/reasoning/permission/done events.

Queued files are consumed by later agent starts. Context files remain in the
server-side context until dropped, matching the TUI `/add` behavior. Submitted
media is copied into `$ZS_DATA_DIR/media/<session-id>/`; its filename, MIME type,
size, and stored filename are linked to the user message in session JSON so the
attachment is displayed and restored into provider history after resume.

Assistant markdown is rendered by zerostack and streamed as events like
`(event :type assistant-render :replace-from N :lines ((:text "< hi" :face zs-normal)))`.
Emacs should delete from `:replace-from` and insert the provided line batch.
Markdown link spans include their destination, for example
`(:text "docs" :face zs-link :url "https://example.com/docs")`, so the client
can open them with `browse-url`. Markdown image spans include their alt text and
path, for example `(:text "plot" :face zs-normal :image "artifacts/plot.png")`.
The bundled Emacs client resolves relative paths against the session worktree and
displays readable, locally supported image files inline, retaining alt text when
loading is unavailable or fails.
Provider usage events include `:reasoning-tokens` when the provider reports
thinking token usage, on both `completion-call` and final `done` events. Final
assistant turns render persisted `thinking:12k` markers, and persisted tool
results include elapsed execution time on the output line, e.g.
`◈ result (12 chars) [1.2s]:`.

Reasoning chunks and short tool outputs are written to files inside the live
session runtime directory instead of being sent inline. Long tool outputs and
display patches are stored under the persistent session data directory, with
their paths recorded in session JSON so reopened sessions use the same artifact
renderer and links. Events include an artifact plist:

```lisp
(event :type tool-result
       :turn 3
       :name "bash"
       :chars 18324
       :preview "first small preview..."
       :artifact (:kind tool-output
                  :path "/run/user/1000/zerostack/sessions/<id>/artifacts/turn-3/0002-bash.txt"
                  :mime "text/plain; charset=utf-8"
                  :bytes 18324
                  :preview "first small preview..."
                  :ephemeral t
                  :expires process-exit))
```

In compact activity, tool output and diff links appear in the details buffer,
matched by call ID. `activity-row` events update the group's first message index
without changing logical transcript line counts; compact `tool-result` events
carry `:compact t` so the client avoids redundant artifact caching.

For detailed protocol rendering, tool output links stay on their matching call row, for example
`◈ bash(output 17.9 KB [1.2s]) cargo test`. Parallel calls are matched by call ID,
not tool name or completion order. Reopened sessions use the same grouped layout.
Agent runs execute up to eight tool calls concurrently, including calls to the
same tool. Calls beyond that limit wait for an execution slot. Dependent
operations must be issued in separate model turns rather than one parallel batch.
Each tool outcome updates its row as soon as execution completes; display does
not wait for slower sibling calls. Rig still commits the full batch to provider
history before requesting the next model turn. Buffered batch events do not
produce duplicate results.
Bash calls create a live output artifact before the command starts and write
combined stdout/stderr to that file while running. A `tool-row` event replaces
only the tool-call row identified by `:message-index`, preserving neighboring
calls, streamed text, and the input draft:

```lisp
(event :type tool-row
       :turn 3
       :message-index 12
       :lines ((:text "◈ bash(live output) cargo test"
                :face zs-tool
                :message-index 12
                :role tool-call
                :artifact (:kind live-tool-output
                           :path "/run/user/1000/zerostack/sessions/<id>/artifacts/turn-3/0002-bash-live-output.txt"
                           :mime "text/plain; charset=utf-8"
                           :bytes 0
                           :preview ""
                           :ephemeral t
                           :expires process-exit))))
```

The final `tool-result` still reports the exact text given back to the agent.
The Emacs client opens `live-tool-output` artifacts with tail auto-revert when
available so the file updates live without streaming output chunks over the
protocol. Live-output, reasoning, and LaTeX artifacts disappear when the session
process exits. Persisted tool-output and display-artifact links remain valid
across restarts and are removed when their session is deleted.

Assistant renders may include LaTeX metadata for inline SVG display. Zerostack
recognizes inline `$...$` and `\(...\)` math plus display `$$...$$` and
`\[...\]` math. Detection runs on sanitized Markdown before layout, excludes
inline and fenced code, and preserves math as literal text while Markdown is
rendered and wrapped. Zerostack writes ephemeral `.tex` source artifacts,
renders them to ephemeral SVG artifacts with `latex` and `dvisvgm` when those
tools are available, marks the rendered line ranges, and emits
`latex-preview-ready` after `done` so the Emacs client can apply stable inline
overlays.

```lisp
(:text "< Inline $x^2$"
 :face zs-normal
 :latex ((:id "turn-3-latex-1"
          :display nil
          :source "x^2"
          :line-start 42
          :col-start 9
          :line-end 42
          :col-end 14
           :artifact (:kind latex-source
                      :path "/run/user/1000/zerostack/sessions/<id>/artifacts/turn-3/latex-0001.tex"
                      :mime "text/x-tex; charset=utf-8"
                      :bytes 212
                      :preview "\\documentclass{article} ..."
                      :ephemeral t
                      :expires process-exit)
           :svg-artifact (:kind latex-svg
                          :path "/run/user/1000/zerostack/sessions/<id>/artifacts/turn-3/latex-0001.svg"
                          :mime "image/svg+xml"
                          :bytes 1872
                          :preview "<?xml version='1.0' ..."
                          :ephemeral t
                          :expires process-exit)
           :error nil)))

(event :type latex-preview-ready
       :turn 3
       :items ((:id "turn-3-latex-1" ...)))
```

The intended Emacs behavior is: insert the pre-rendered markdown lines, collect
`:latex` items, wait for `latex-preview-ready`, then create overlays for those
line/column ranges. The client prefers `:svg-artifact` and displays it strictly
in place via an image overlay. If SVG rendering is missing or unavailable,
`:svg-artifact` is nil and `:error` contains the command, timeout, or compilation
failure; the client exposes that error in the overlay help and can fall back to
the older off-screen AUCTeX/`TeX-fold-mode` string display. Source `.tex`
artifact buffers are not displayed automatically; `/latex` or artifact actions
open them explicitly.

## Native Emacs Board Snapshot

`zerostack --emacs-board` is a lightweight, non-agent command for Emacs. It
reads saved session JSON, checks the native Emacs live-session registry, asks Git
for canonical repos and worktrees, prints one S-expression to stdout, and exits
before provider/client initialization. It retains only board metadata and reduces
messages to their count, latest user title, and latest nonzero assistant usage;
tool payloads, reasoning, and other unused fields are skipped during parsing.
The JSON files are still scanned in full, without building full session objects.
Git repository lookups are cached per working directory for each snapshot,
including non-repository results; the cache is discarded after the refresh.

The bundled client's board refreshes run asynchronously, including refreshes
triggered by session events. Refresh requests while a fetch is running are
ignored; no follow-up fetch is queued. The previous board remains usable until
the new snapshot arrives; failed refreshes retain it and report an error in
`*Messages*`.

When built with the `veles` feature, `S` runs an asynchronous semantic search
across saved conversations. The persistent index under
`$XDG_CACHE_HOME/zerostack/session-search/` contains user messages, assistant
messages, and compaction summaries, but excludes tool calls, tool results, and
reasoning. Results open the matching saved session. The first search may need
to build the local embedding index.

The snapshot shape is:

```lisp
(zerostack-board
 :version 1
 :needs-attention
 ((:id "..."
   :short-id "12345678"
   :title "ready session"
   :cwd "/repo/zerostack"
   :model "..."
   :provider "..."
   :created-at "..."
   :updated-at "..."
   :message-count 12
   :tokens 3400
   :cost 0.012300
   :alive t
   :pid 12345
   :socket "/run/user/1000/zerostack/sessions/<id>/sock"))
 :projects
 ((:name "zerostack"
   :path "/repo/zerostack"
   :repo "/repo/zerostack/.git"
   :alive t
   :updated-at "2026-06-20T00:00:00Z"
   :worktrees
   ((:path "/repo/zerostack"
     :branch "main"
     :description "branch description from git config"
     :alive t
     :sessions
     ((:id "..."
       :short-id "12345678"
       :title "last user prompt or session name"
       :cwd "/repo/zerostack/subdir"
       :model "..."
       :provider "..."
       :created-at "..."
       :updated-at "..."
       :message-count 12
        :tokens 3400
        :cost 0.012300
        :alive t
        :pid 12345
        :socket "/run/user/1000/zerostack/sessions/<id>/sock"))))))
 :loose-workspaces
 ((:path "/scratch/not-a-git-repo"
   :alive nil
   :updated-at "2026-06-19T00:00:00Z"
   :sessions
   ((:id "..."
     :short-id "87654321"
     :title "non-git notes"
     :cwd "/scratch/not-a-git-repo"
     :model "..."
     :provider "..."
     :created-at "..."
     :updated-at "..."
     :message-count 3
     :tokens 500
     :cost 0.000000
     :alive nil
     :pid nil
     :socket nil))))
```

`:needs-attention` contains saved sessions that completed a native Emacs turn
since they were last opened. Opening the session removes it from the section;
the inline `dismiss` button runs `zerostack --emacs-dismiss-attention <id>` to
remove it without opening.

Projects are canonical Git repos, keyed by Git common dir. Project children are
the worktrees Git reports for that repo and which still exist in the filesystem,
including worktrees that currently have no sessions. Worktree rows in Emacs show
only the branch description, branch name, and a compact directory marker.
Session rows are saved zerostack sessions whose `working_dir` belongs to that
worktree; Emacs displays only the title and how long ago the session was last
updated. Sessions whose `working_dir` no longer exists are omitted. Sessions
whose `working_dir` is not inside a Git repository are grouped under
`:loose-workspaces` and rendered as a separate "other workspaces" section.
Projects, worktrees, loose workspaces, and sessions with live native Emacs
sessions sort before inactive ones; sessions are then sorted by most-recent
update. Each worktree/workspace initially renders five sessions and adds a
clickable `show 5 more` row when more are available.

## Standalone workspace CLI

With the `git-worktree` feature, create a workspace without Emacs:

```bash
zerostack workspace create --repo /path/to/repo --branch task \
  --path /path/to/workspace --description "Task workspace"
```

On Unix, creation runs in a detached worker, including `prepare`, worktree
creation, and `hydrate`. The command returns JSON containing `job`, `path`,
`branch`, `status`, `log`, and `error` after worker startup, without waiting for
hydration. Submission success does not mean the workspace is ready.

```bash
zerostack workspace status --job FULL-JOB-UUID
zerostack workspace wait --job FULL-JOB-UUID --timeout 600
zerostack workspace logs --job FULL-JOB-UUID
```

Use `workspace wait` instead of repeatedly polling before starting a session.
It prints concise phase changes and a progress heartbeat every ten seconds to
stderr, leaving stdout for one final JSON result. It exits successfully only for
`ready`; failure, interruption, and timeout return nonzero. `--timeout` is in
seconds and defaults to 600. A timeout adds `timed_out: true` to the result but
does not cancel setup; inspect the same job or wait again rather than resubmitting.
A zero timeout performs one readiness check.

New jobs include `created_at_ms`, `finished_at_ms`, `elapsed_ms`, and `phases`.
Each phase records its name, start/end timestamps, and `duration_ms`; active
phase and total durations update on status/wait reads, and freeze for recorded
ready/failed results. Interrupted-job timings end at each query's detection time,
not a recorded worker-exit time. Timestamps are Unix milliseconds; durations use
wall time and saturate at zero
if the clock moves backwards. Older records remain readable with null total
timing and no phase history; no historical durations are fabricated.

Status phases are `queued`,
`preparing`, `creating`, and `hydrating`; terminal outcomes are `ready`, `failed`,
and `interrupted`. A dead worker without a recorded result reports `interrupted`,
not success. Status queries return JSON even for failed jobs: inspect `status`
and `error`, not just the CLI exit code. Logs return all combined hook stdout
and stderr collected so far; tail the returned log path for live output.
Jobs persist under the zerostack data directory's `workspace-jobs/<job>/`.
Worker liveness uses an OS file lock, avoiding Unix socket address-length limits
in long data-directory paths. Older socket-based workers remain recognized.
If submission times out, inspect those records before retrying creation.
Failures do not roll back existing worktrees or partially completed hydration.

Relative paths are resolved against the repository. The base defaults to
`origin/HEAD`; use `--base HEAD` for repositories without that remote reference.
Existing paths and invalid branch names are rejected. On non-Unix platforms,
creation remains synchronous; detached status/log commands are Unix-only.
Existing TUI worktree creation behavior is unchanged.

On Unix, optionally wait for setup and start a session in one command:

```bash
zerostack workspace create --repo /path/to/repo --branch task \
  --path /path/to/workspace --start-session --timeout 600 \
  --provider openai --model gpt-4o-mini --prompt "Implement the task"
```

`--start-session` waits for `ready` before using the same persistent session
launcher as `session start`. Omit `--prompt` for an idle session. `--prompt`,
`--provider`, `--model`, and `--timeout` require `--start-session`; a blank prompt
is rejected before submission. The setup wait defaults to 600 seconds; session
startup has its separate 30-second deadline. Stderr reports the job ID/log path
at submission and concise progress while waiting. Successful stdout is one JSON
object with `workspace` (completed job and timings) and `session` (session startup
metadata). Prompt acceptance does not imply task completion.

Setup failure/interruption/timeout prints the final job JSON and returns nonzero
without starting a session. Timed-out setup continues independently; wait on that
job and use `session start` afterwards, rather than creating the workspace again.
If session startup or the initial prompt fails after successful setup, stdout
contains `workspace`, `session: null`, and `session_error`, and the command returns
nonzero. The workspace is retained; an initial-prompt failure can leave a live
session, whose startup metadata is included in the error. Do not blindly retry.

Inspect the board and launch persistent workers without Emacs:

```bash
zerostack board list
zerostack session start --path /path/to/workspace --prompt "Implement the task"
zerostack session send --session FULL-UUID --prompt "Follow-up task"
```

`board list` returns JSON using the same snapshot as the Emacs board; its default
output is unchanged. Narrow it before asking an agent to inspect a large board:

```bash
zerostack board list --summary
zerostack board list --repo /path/to/repo --alive --summary
zerostack board list --path /path/to/workspace
zerostack board list --session UUID-OR-PREFIX
```

Filters combine with AND. `--repo` accepts the repository root or any of its Git
worktrees, matching their shared Git common directory. `--path` matches an exact
board worktree or loose workspace path, not descendants; paths are canonicalized
so relative paths and symlinks work. `--session` matches all IDs starting with the
supplied prefix; no matches returns empty groups, not an error. `--alive` retains
only live sessions. Session/alive filters prune empty groups and attention rows;
repository/path filters alone can retain worktrees with no sessions.

`--summary` omits session records, including duplicate attention records. Its
`counts` object reports projects, workspaces, sessions, live sessions, and
attention sessions after filtering. Each worktree/loose workspace has
`session_count` and `alive_session_count`; worktrees keep branch/description
metadata. Use a filtered non-summary query to get session IDs, sockets, and other
session details. Summaries reduce output, not the underlying snapshot collection work.
On Unix, `session start` uses the shared Rust daemon launcher to start a separate
systemd user service and returns JSON with `session`, `pid`, `path`, `socket`,
`log`, and `unit` after a readiness handshake. It accepts optional `--provider`,
`--model`, and `--session ID-PREFIX` to resume a saved session; omit `--prompt`
for an idle worker. An already-running session is rejected rather than duplicated.
Startup has a 30-second deadline; failed startup reports the private log path,
and a timed-out service is stopped. Logs are retained under
`<data-dir>/session-logs/` with private file permissions.
Workers retain normal permissions and may wait for approval through a socket
client such as Emacs. The process continues after the CLI exits.

`session send` requires the full UUID and acknowledges acceptance, not completion.
Busy workers reject new prompts. A timed-out send may already have been accepted;
inspect the session before retrying. Logs contain diagnostics, not the transcript.
For a single foreground turn, use `cd /path/to/workspace && zerostack -p "Task"`.

The embedded `zerostack-board` skill is automatically made available to agents
with tools enabled. Its instructions cover workspace creation, worker startup,
follow-up prompts, and permission/timeout precautions. User skills with the
same name take precedence.

## Native Emacs Client

The repository includes an Emacs Lisp client at `emacs/zerostack.el`. It is
ERC-style: normal input is sent as a prompt, and client/protocol actions are
available from a small Hydra command menu. The menu is intentionally limited to
actions that need explicit client UI: skills, file/clipboard attachments,
provider/model switching, compaction, loop control, render width, and latest
artifact. Dynamic actions such as selecting a skill proceed to a second selection
prompt. Permission requests render as inline buttons below the input prompt, so
they do not require the command menu.

Load it from a checkout:

```elisp
(add-to-list 'load-path "/path/to/zerostack/emacs")
(require 'zerostack)
```

Both Emacs and `zerostack session start` use the same Rust-owned launcher in
`src/extras/session_cli.rs`. Emacs invokes `zerostack --emacs-launch` with its
existing startup arguments, then connects to the reported socket; Lisp does not
construct systemd commands or independently configure daemon processes.
The shared launcher creates separate transient user services using
`systemd-run --user` (requires systemd-run and an active user systemd manager).
Each daemon has its own cgroup, so restarting `emacs.service` does not kill it.
The working directory and Emacs environment are preserved; environment values
are inherited without placing credentials in launcher command-line arguments.
Stdin is detached and stdout/stderr go to a private retained session log file.
Closing a chat buffer, disconnecting, or exiting Emacs leaves the daemon running.
No permanent unit files or user-manager configuration are installed.
Reopen the board and press `RET` on a live session to reattach. Use the board's
`s` action to explicitly stop a daemon, or the chat menu's `restart` action to
replace it. Sessions do not automatically restart after logout or reboot.
Startup failures are reported in the chat with the retained log path. The shared
Rust launcher enforces the 30-second startup deadline and stops timed-out units;
the Emacs client has a 35-second fallback notification timeout.
Reload the updated Lisp and restart existing daemons to move them out of the
Emacs service's cgroup; Unix `setsid` alone does not provide this isolation.
The board reads daemon-published activity (`idle`, `running`, or
`waiting-permission`) independently of open chat buffers. Active loops and
unfinished prompt drivers remain busy between turns. Older daemons that do not
publish activity count as idle for bulk restart, as a compatibility policy.

Main entry points:

| Command | Description |
| ------- | ----------- |
| `M-x zerostack` | Start `zerostack --emacs`, wait for its socket, connect, and attach. With a prefix argument, read extra CLI args. The command menu includes `restart` to restart the buffer's daemon without closing the buffer. |
| `M-x zerostack-connect` | Connect to an existing session socket. |
| `M-x zerostack-list-sessions` | Run `zerostack --emacs-list` in a sessions buffer. |
| `M-x zerostack-board` | Run `zerostack --emacs-board` and render a project/worktree/session tree. |

Key bindings in `zerostack-board-mode`:

| Key | Action |
| --- | ------ |
| `g` | Refresh the board snapshot. |
| `S` | Prompt for a semantic search across saved conversations (requires the `veles` feature), then show asynchronously loaded results. `g` reruns the query in the results buffer; `RET` opens its session. |
| `RET` | Open the item at point. Projects/worktrees open with `dired`; live sessions connect to their socket; inactive sessions start `zerostack --emacs --session <id>`. Subagent lists start collapsed; opening a child session uses its isolated workspace as `default-directory`. Needs-attention rows also have a clickable `dismiss` button. |
| `c` | Create from the item at point. On a project, prompts for a branch/path/description, runs the local workspace `prepare` hook, and creates a worktree from `origin/HEAD`; the path defaults to `<repo>_<branch>`. The `hydrate` hook then runs without blocking in `*zerostack hydrate: <branch>*`. On a worktree, starts a new `zerostack --emacs` session with that worktree as `default-directory`. |
| `p` | Persist a new default provider in zerostack config. The model is reset to that provider's configured/default model. |
| `m` | Persist a new default model in zerostack config for the current default provider. |
| `s` | Stop the live session process at point after confirmation. |
| `x` | Remove the worktree or session at point after confirmation. Worktrees use Emacs trash and then `git worktree prune`; deleting a subagent session also removes its isolated Git worktree or copied workspace. |

Key bindings in `zerostack-mode`:

| Key | Action |
| --- | ------ |
| `RET` | Send current input. |
| `C-c C-c` | Abort the active turn. |
| `C-c C-m`, `C-c /` | Open the Hydra command menu, including `restart` for the current daemon. |
| `C-c C-a` | Attach/render the full session snapshot. |
| `C-c C-o` | Open artifact or LaTeX source at point. |
| `C-c C-s` | Request session status. |

Command menu actions:

| Action | Description |
| ------ | ----------- |
| `restart` | Restart the current buffer's `zerostack --emacs` daemon and reconnect without closing the buffer, including externally attached sessions. Attached daemon PIDs are verified against their socket's registration; restart waits up to five seconds for exit before launching a replacement. |
| `hydrate` | Rerun the current worktree's local `hydrate` hook asynchronously in a visible shell-command buffer. |
| `view` | Change server-side markdown render width. |
| `attach` | Add a file by path, attach image data from the clipboard, list queued attachments, or drop all queued attachments. |
| `provider` | Switch the live session provider. This is session-local and does not rewrite config. |
| `model` | Switch the live session model for the current provider. This is session-local and does not rewrite config. |
| `compact` | Ask zerostack to compact history, then rerender the buffer. |
| `loop` | Start or stop the iterative loop. Starting prompts for objective, optional max iterations, and optional validation command. |
| `skill` | Discover runtime skills from the same home/project skill roots and insert an explicit selected-skill directive into the input line. |
| `artifact` | Open the most recent artifact. |
| `log` | Open the stdout/stderr log file for a daemon launched by this buffer. |

The `attach` action sends `file-add` for path-based files. Clipboard attachment
accepts actual PNG, JPEG, GIF, or WebP image data only; clipboard text, including
paths and file URIs, is pasted normally. Emacs reads image data through GUI
selection targets and falls back to `wl-paste`, `xclip`, or `pngpaste` when
available. On prompt submission, zerostack copies the image into the session's
media directory and links it to that user turn. `zerostack-mode` also registers
an Emacs `yank-media` handler for image MIME types, so `M-x yank-media` and
`C-c / -> attach -> clipboard` use the native Emacs media clipboard path before
falling back to lower-level image target and command probing.

Other operations use direct keys instead: `C-c C-c` aborts, `C-c C-a` attaches or
rerenders the full snapshot, `C-c C-s` requests status, and `C-c C-o` opens the
artifact at point.

When a loop is active, the prompt shows `zs loop>` or `zs loop thinking>`. `C-c C-c`
aborts the active loop turn and stops the loop so the client will not schedule the
next iteration.

Slash-prefixed text in the input line is no longer special; it is sent as a
normal prompt. Use the command menu for client actions.

Chat buffers are named from the session title and worktree directory, for
example `*zerostack: Fix parser @ parser-worktree*`. A directly connected
buffer renames itself when the worker reports session metadata. Opening the same
session again reuses the existing chat buffer instead of creating another buffer;
the client matches by session id first and socket path second.

The chat buffer only inserts server-rendered transcript lines from render events.
Routine client notices such as `ready`, `tool-result`, completion usage, and
`done` are not added as extra buffer lines. Thinking/actionable status is kept on
the single prompt line instead. Permission requests additionally show `allow
once`, `allow always`, and `deny` buttons below the input prompt until answered.

The Nix dev shell includes Emacs with SVG image support plus TeX/dvisvgm tooling.
Run client tests with:

```bash
nix develop --no-write-lock-file -c emacs --batch -Q -L emacs -l zerostack -l zerostack-test -f ert-run-tests-batch-and-exit
```

Those ERT tests exercise all client-side outbound protocol commands, every
server form/event handled by the client, command menu dispatch, board rendering/actions,
artifacts, Rust-rendered inline LaTeX SVG metadata, render replacement, and a Unix-socket end-to-end
round trip against a mock native server. The socket test intentionally avoids
model/provider calls so it can run without API credentials.

## Native Emacs Demo Example

The live demo is an example binary, not a `zerostack` CLI mode:

```bash
nix run .#demo
```

The flake app provides a graphical Emacs build with SVG image support and
TeX/dvisvgm for the demo. Set `EMACS=/path/to/emacs` only if you want to
override that executable.

It creates a temporary isolated environment with its own `ZS_DATA_DIR`,
`ZS_RUNTIME_DIR`, and `ZS_CONFIG_DIR`; writes dummy Git projects, worktrees, and
saved session JSON; starts a tiny local OpenAI-compatible HTTP server; writes a
normal custom-provider config pointing at that server; starts ordinary
`zerostack --emacs --provider demo-openai --model zerostack-demo-random`
workers; launches graphical Emacs on `M-x zerostack-board`; connects to one live session;
and sends an initial delayed prompt that exercises rendered markdown, tool
artifacts, Rust-rendered inline LaTeX SVGs, permission requests, and interruption.
The app uses a Nix-built regular `zerostack` binary with the `multimodal` and
`rtk` features enabled. The dummy projects also contain project-local skills under
`.claude/skills` and `.opencode/skills`, so the workers discover them through the
same normal skill pipeline as real sessions. The saved data includes one huge
transcript, one crowded worktree with more than ten sessions for board pagination,
and non-Git workspaces so the board's separate `other workspaces` category is
visible.

The regular `zerostack` binary sees only normal configuration and normal custom
provider traffic. There are no production demo provider branches or special demo
hatches. The mock provider streams delayed provider reasoning so the chat buffer
shows that the agent is thinking and `C-c C-c` can abort an active turn. The
auto-opened live worker runs with `--restrictive`, so the first tool call asks for
permission; the remaining built-in demo tools are pre-allowed in that isolated
seeded session so the demo only shows one permission request. Answer it with the
inline buttons below the input prompt. It walks
through the built-in tools it is offered (`read`, `list_dir`, `find_files`,
`grep`, `task` subagent, `write`, `edit`, `bash`, and `write_todo_list`),
executes one bash command through the demo RTK path and a second bash command
with `disable_rtk: true`, deliberately emits one slow 30-second raw bash result so
the live output artifact visibly tails in Emacs, and reads that saved path back through the normal `read`
tool before returning markdown containing tables, task lists, code, links, and
LaTeX so the native Emacs client can show rendered lines, project-local skill
discovery, ephemeral thinking/tool artifacts, saved-output readback, and inline
SVG math.

The demo does not auto-attach a file. If you manually use `C-c / -> attach` and
then send a prompt, the local OpenAI-compatible provider detects the incoming
media/file content, writes it under the isolated demo attachment dump directory,
and responds with the saved paths instead of entering the regular multi-tool loop.
Retrying after abort uses unique demo write/edit paths, so partially-created files
from an interrupted turn do not break the next prompt. The temporary environment
is removed when the example exits. The Nix shell includes TeX/dvisvgm tooling for
rendering SVG artifacts and inspecting LaTeX sources. Set
`ZEROSTACK_DEMO_DELAY_MS=<ms>` to tune provider delay. Set
`ZEROSTACK_BIN=/path/to/zerostack` only to override the Nix-built demo binary, or
`EMACS=/path/to/emacs` to override the graphical Emacs from the flake app.
Set `ZEROSTACK_DEMO_KEEP=1` to keep the temporary environment and worker logs
after exit for debugging.

## Skills

Zerostack discovers Pi-style skills and injects an `<available_skills>` block into
the model context when tools are enabled. The block lists each visible skill's
name, description, and absolute `SKILL.md` path, and tells the model to use the
`read` tool to load matching instructions. Disabled tools omit the skill block so
the model is not told to read files it cannot access.

Skill roots are directories containing `SKILL.md` with frontmatter that includes a
required `description`; `name` is optional and defaults to the directory name.
`disable-model-invocation: true` keeps a skill out of the model-visible list.

Discovery checks home-level directories:

- `~/.config/opencode/skills`
- `~/.opencode/skills`
- `~/.claude/skills`
- `~/.pi/agent/skills`
- `~/.agents/skills`
- the zerostack config skill directory under `agent/skills`

It also checks project and ancestor directories up to the Git root:

- `.opencode/skills`
- `.claude/skills`
- `.pi/skills`
- `.agents/skills`

Hidden directories and `node_modules` are skipped. Duplicate skill names keep the
first discovered skill.

## Prompt Shortcut

Prefix a message with `.` to quickly switch prompts or run a one-shot query with
a different prompt.

| Example | Description |
| ------- | ----------- |
| `.` | Open the prompt picker (same as `/prompt` picker). |
| `.ask` | Switch to the `ask` prompt (same as `/prompt ask`). |
| `.plan what files changed?` | Temporarily use the `plan` prompt for this query, then restore the previous prompt and security mode. |

The `.[prompt] [msg]` syntax is a one-shot: it sets the prompt, submits the
message, and after the response restores the previous prompt and
`last_user_mode`.

## General

With the `multimodal` feature enabled, the `read` tool fully decodes and validates
PNG, JPEG, static GIF, and static WebP files before returning image tool results.
Animated images are rejected. Encoded images are limited to 20 MB, dimensions to
16,384 pixels per edge and 100 megapixels, and decoder allocation to 400 MB.
Images are copied into session-owned media storage and revalidated when replaying
saved tool history; invalid stored images become warning text instead of being
resent. Image read results are sent as a text tool result followed by a top-level
image message because OpenAI function-call outputs only accept text. The selected
provider and model must support image input.

| Command | Description |
| ------- | ----------- |
| `/help` | Show the full help message listing all commands and keybindings. |

## Keybindings

| Shortcut | Action |
| -------- | ------ |
| `Enter` | Send message. |
| `Shift+Enter` | Insert newline. |
| `Ctrl+C` | Cancel current agent response or quit. |
| `Ctrl+D` | Send message (alternative). |
| `Ctrl+W` | Delete word backwards. |
| `Ctrl+U` | Delete to beginning of line. |
| `Ctrl+L` | Clear terminal. |
| `Ctrl+G` | Open the current input in the system editor (`$EDITOR`). |
| `Ctrl+H` | Launch `lazygit` (git TUI) in the project directory. |
| `Ctrl+S` | Save session. |
| `Tab` | Activate file picker / auto-complete paths. |
| `Up / Down` | Navigate command history. |
| `PageUp / PageDown` | Scroll viewport. |
| `Home / End` | Jump to start/end of input. |
| `Alt+Enter` | Retry last prompt. |
| `Escape` | Close active picker / cancel. |
