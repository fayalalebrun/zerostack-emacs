# Slash Commands

All slash commands are available from the TUI input prompt.

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
| `/history` | Show global chat history (last 10 entries across sessions). |
| `/fork [message-index]` | Fork the current conversation into a new session before a selected user message, or before `message-index`. |

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

When a loop is active, one-off `(prompt ...)` commands are rejected until the loop
is stopped. Loop events are broadcast as ordinary protocol events:
`loop-started` when the loop is accepted, `loop-iteration` before each agent
iteration, and `loop-stopped` with `:reason stopped` or `:reason max`. Each loop
iteration still emits normal render/tool/reasoning/permission/done events.

Queued files are consumed by later agent starts. Context files remain in the
server-side context until dropped, matching the TUI `/add` behavior. Media
attachments are held in memory and drained into the next prompt's Rig history;
they are not persisted in session JSON.

Assistant markdown is rendered by zerostack and streamed as events like
`(event :type assistant-render :replace-from N :lines ((:text "< hi" :face zs-normal)))`.
Emacs should delete from `:replace-from` and insert the provided line batch.

Reasoning chunks and final tool outputs are written to files inside the live
session runtime directory instead of being sent inline. Events include an
artifact plist:

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

Rendered lines may also carry `:artifact`, for example
`(:text "  output: bash (17.9 KB)" :face zs-link :artifact (...))`. Bash tool
calls in native Emacs sessions also create a live output artifact before the
command starts and write combined stdout/stderr to that file while the command is
running:

```lisp
(event :type tool-render
       :turn 3
       :replace-from 12
       :lines ((:text "  live output: cargo test"
                :face zs-link
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
protocol. Artifacts are not persisted in session JSON and disappear on normal
process exit when the session runtime directory is removed. Crash cleanup is
best-effort via runtime directory lifetime and stale session sweeping.

Assistant renders may include LaTeX metadata for inline SVG display. Zerostack
writes ephemeral `.tex` source artifacts, renders them to ephemeral SVG artifacts
with `latex` and `dvisvgm` when those tools are available, marks the rendered
line ranges, and emits `latex-preview-ready` after `done` so the Emacs client can
apply stable inline overlays.

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
                          :expires process-exit))))

(event :type latex-preview-ready
       :turn 3
       :items ((:id "turn-3-latex-1" ...)))
```

The intended Emacs behavior is: insert the pre-rendered markdown lines, collect
`:latex` items, wait for `latex-preview-ready`, then create overlays for those
line/column ranges. The client prefers `:svg-artifact` and displays it strictly
in place via an image overlay. If SVG rendering is missing or unavailable, it can
fall back to the older off-screen AUCTeX/`TeX-fold-mode` string display. Source
`.tex` artifact buffers are not displayed automatically; `/latex` or artifact
actions open them explicitly.

## Native Emacs Board Snapshot

`zerostack --emacs-board` is a lightweight, non-agent command for Emacs. It
loads saved session JSON, checks the native Emacs live-session registry, asks Git
for canonical repos and worktrees, prints one S-expression to stdout, and exits
before provider/client initialization.

The snapshot shape is:

```lisp
(zerostack-board
 :version 1
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

Projects are canonical Git repos, keyed by Git common dir. Project children are
the worktrees Git reports for that repo and which still exist in the filesystem,
including worktrees that currently have no sessions. Worktree rows in Emacs show
only the branch description, branch name, and a compact directory marker.
Session rows are saved zerostack sessions whose `working_dir` belongs to that
worktree; Emacs displays only the title and how long ago the session was last
updated. Sessions whose `working_dir` is not inside a Git repository are grouped
under `:loose-workspaces` and rendered as a separate "other workspaces" section.
Projects, worktrees, loose workspaces, and sessions with live native Emacs
sessions sort before inactive ones; sessions are then sorted by most-recent
update. Each worktree/workspace initially renders five sessions and adds a
clickable `show 5 more` row when more are available.

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

Main entry points:

| Command | Description |
| ------- | ----------- |
| `M-x zerostack` | Start `zerostack --emacs`, wait for its socket, connect, and attach. With a prefix argument, read extra CLI args. |
| `M-x zerostack-connect` | Connect to an existing session socket. |
| `M-x zerostack-list-sessions` | Run `zerostack --emacs-list` in a sessions buffer. |
| `M-x zerostack-board` | Run `zerostack --emacs-board` and render a project/worktree/session tree. |

Key bindings in `zerostack-board-mode`:

| Key | Action |
| --- | ------ |
| `g` | Refresh the board snapshot. |
| `RET` | Open the item at point. Projects/worktrees open with `dired`; live sessions connect to their socket; inactive sessions start `zerostack --emacs --session <id>`. |
| `c` | Create from the item at point. On a project, prompts for a branch/path/description and runs `git worktree add` from Emacs. On a worktree, starts a new `zerostack --emacs` session with that worktree as `default-directory`. |
| `p` | Persist a new default provider in zerostack config. The model is reset to that provider's configured/default model. |
| `m` | Persist a new default model in zerostack config for the current default provider. |
| `s` | Stop the live session process at point after confirmation. |
| `x` | Move the worktree or session at point to trash after confirmation. Worktrees use Emacs trash and then `git worktree prune`; sessions move their JSON file to trash. |

Key bindings in `zerostack-mode`:

| Key | Action |
| --- | ------ |
| `RET` | Send current input. |
| `C-c C-c` | Abort the active turn. |
| `C-c C-m`, `C-c /` | Open the Hydra command menu. |
| `C-c C-a` | Attach/render the full session snapshot. |
| `C-c C-o` | Open artifact or LaTeX source at point. |
| `C-c C-s` | Request session status. |

Command menu actions:

| Action | Description |
| ------ | ----------- |
| `view` | Change server-side markdown render width. |
| `attach` | Add a file by path, attach clipboard file/image/text contents, list queued attachments, or drop all queued attachments. |
| `provider` | Switch the live session provider. This is session-local and does not rewrite config. |
| `model` | Switch the live session model for the current provider. This is session-local and does not rewrite config. |
| `compact` | Ask zerostack to compact history, then rerender the buffer. |
| `loop` | Start or stop the iterative loop. Starting prompts for objective, optional max iterations, and optional validation command. |
| `skill` | Discover runtime skills from the same home/project skill roots and insert an explicit selected-skill directive into the input line. |
| `artifact` | Open the most recent artifact. |

The `attach` action sends `file-add` for path-based files. Clipboard paste first
tries to interpret the clipboard as a plain path, `file://` URI, `text/uri-list`,
or desktop copied-file target such as `x-special/gnome-copied-files`. If image
bytes are available, Emacs writes them to a temporary image file and attaches
that path; as a final fallback it writes clipboard text to a temporary `.txt`
file. Clipboard reads use Emacs GUI selection targets first and fall back to
common platform commands such as `wl-paste`, `xclip`, `pbpaste`, and `pngpaste`
when available. Temporary clipboard files are kept for the session and removed
on disconnect. `zerostack-mode` also registers an Emacs `yank-media` handler for
image MIME types, so `M-x yank-media` and `C-c / -> attach -> clipboard` use the
native Emacs media clipboard path before falling back to the lower-level target
and command probing.

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
