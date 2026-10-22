use std::io::Read;

use crate::ui::events::render_session;
use crate::ui::slash::{SlashCtx, undo_last, write_error, write_ok, write_result};

pub async fn handle(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    match parts[0] {
        "/sessions" => handle_sessions(parts, ctx).await,
        "/clear" | "/new" => handle_clear(ctx).await,
        "/undo" => handle_undo(ctx).await,
        "/redo" => handle_redo(ctx).await,
        "/retry" => handle_retry(ctx).await,
        "/quit" | "/exit" => handle_quit(ctx).await,
        "/history" => handle_history(ctx).await,
        "/rewind" => handle_rewind(parts, ctx).await,
        "/fork" => handle_fork(parts, ctx).await,
        _ => Ok(()),
    }
}

async fn handle_fork(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    anyhow::ensure!(
        !*ctx.is_running,
        "cannot fork while an agent turn is running"
    );
    #[cfg(feature = "loop")]
    anyhow::ensure!(
        !ctx.loop_state.as_ref().is_some_and(|state| state.active),
        "cannot fork while a loop is active"
    );
    anyhow::ensure!(
        !ctx.cli.no_session,
        "fork requires session persistence; --no-session is active"
    );
    anyhow::ensure!(
        ctx.cli.subagent_parent_session.is_none(),
        "fork requires a main session"
    );
    anyhow::ensure!(parts.len() <= 2, "usage: /fork [message-index]");
    let before = parts
        .get(1)
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| anyhow::anyhow!("usage: /fork [message-index]"))?;
    let mut fork = crate::session::fork::save(ctx.session, before)?;
    // Resolve credentials before replacing live state. On failure the saved fork
    // remains available, and the original session remains active.
    let client = crate::provider::create_client(
        &fork.provider,
        ctx.cli.api_key.as_deref(),
        &ctx.cfg.custom_providers_map(),
        ctx.cfg.api_keys.as_ref(),
        Some(fork.id.as_str()),
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "fork {} was saved but could not be activated: {error}",
            fork.id
        )
    })?;
    let text = before
        .map(|index| ctx.session.messages[index].content.clone())
        .unwrap_or_default();
    #[cfg(feature = "multimodal")]
    if let Some(index) = before {
        fork.pending_media = ctx.session.messages[index]
            .attachments
            .iter()
            .map(|attachment| {
                crate::extras::multimodal::load_persisted_attachment(&fork.id, attachment)
            })
            .collect::<Result<Vec<_>, _>>()?;
    }
    let original_id = ctx.session.id.clone();
    *ctx.session = fork;
    *ctx.client = client;
    *ctx.reasoning_enabled = ctx.session.reasoning_enabled;
    crate::agent::tools::goal::clear_goal_state();
    if let Some(permission) = ctx.permission {
        permission
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reset_session(&ctx.session.id);
    }
    #[cfg(feature = "subagents")]
    crate::extras::subagents::set_parent_session_id(&ctx.session.id);
    crate::agent::tools::set_active_session_id(Some(ctx.session.id.to_string()));
    crate::agent::tools::reset_read_context_loaded(ctx.session.loaded_read_context_paths());
    ctx.rebuild_agent().await;
    render_session(ctx.renderer, ctx.session, ctx.cli, ctx.cfg, ctx.context)?;
    ctx.input.buffer = text;
    ctx.input.cursor = ctx.input.buffer.len();
    write_ok(
        ctx.renderer,
        format!(
            "forked {} → {} (same working directory)",
            &original_id[..8],
            &ctx.session.id[..8]
        ),
    );
    Ok(())
}

async fn handle_sessions(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    if parts.len() < 2 {
        let sessions = crate::session::storage::find_recent_sessions(20)?;
        if sessions.is_empty() {
            write_ok(ctx.renderer, "no saved sessions");
        } else {
            write_ok(
                ctx.renderer,
                format!("recent sessions ({}):", sessions.len()),
            );
            for s in &sessions {
                let last = s
                    .messages
                    .last()
                    .map(|m| format!("...{}", &m.content.chars().take(30).collect::<String>()))
                    .unwrap_or_default();
                let time = crate::ui::events::format_time(&s.updated_at);
                write_result(
                    ctx.renderer,
                    format!(
                        "  {}  {}  {}msgs  {}  {}",
                        &s.id[..8],
                        time,
                        s.messages.len(),
                        s.model,
                        last
                    ),
                );
            }
        }
    } else if parts[1] == "delete" && parts.len() >= 3 {
        let prefix = parts[2].trim();
        let sessions = crate::session::storage::find_sessions_by_prefix(prefix)?;
        if sessions.is_empty() {
            write_ok(ctx.renderer, format!("no session matching '{}'", prefix));
        } else if sessions.len() == 1 {
            if let Some(s) = sessions.into_iter().next() {
                let id = s.id.clone();
                let preview = s
                    .messages
                    .last()
                    .map(|m| format!("...{}", &m.content.chars().take(40).collect::<String>()))
                    .unwrap_or_default();
                if let Err(e) = crate::session::storage::delete_session(&id) {
                    write_error(ctx.renderer, format!("failed to delete: {}", e));
                } else {
                    write_ok(
                        ctx.renderer,
                        format!("deleted session {} {}", &id[..8], preview),
                    );
                }
            }
        } else {
            write_ok(
                ctx.renderer,
                format!("multiple sessions match '{}', be more specific", prefix),
            );
            for s in &sessions {
                let last = s
                    .messages
                    .last()
                    .map(|m| format!("...{}", &m.content.chars().take(30).collect::<String>()))
                    .unwrap_or_default();
                let time = crate::ui::events::format_time(&s.updated_at);
                write_result(
                    ctx.renderer,
                    format!(
                        "  {}  {}  {}msgs  {}  {}",
                        &s.id[..8],
                        time,
                        s.messages.len(),
                        s.model,
                        last
                    ),
                );
            }
        }
    } else {
        let prefix = parts[1].trim();
        let sessions = crate::session::storage::find_sessions_by_prefix(prefix)?;
        if sessions.is_empty() {
            write_ok(ctx.renderer, format!("no session matching '{}'", prefix));
        } else if sessions.len() == 1 {
            if let Some(mut s) = sessions.into_iter().next() {
                // Resolve the client before replacing any live session state.
                let client = crate::provider::create_client(
                    &s.provider,
                    ctx.cli.api_key.as_deref(),
                    &ctx.cfg.custom_providers_map(),
                    ctx.cfg.api_keys.as_ref(),
                    Some(s.id.as_str()),
                )?;
                s.initialize_service_tier(ctx.cli, ctx.cfg);
                let msg_count = s.messages.len();
                *ctx.session = s;
                *ctx.client = client;
                *ctx.reasoning_enabled = ctx.session.reasoning_enabled;
                ctx.rebuild_agent().await;
                render_session(ctx.renderer, ctx.session, ctx.cli, ctx.cfg, ctx.context)?;
                write_ok(ctx.renderer, format!("loaded session ({} msgs)", msg_count));
            }
        } else {
            write_ok(
                ctx.renderer,
                format!("multiple sessions match '{}':", prefix),
            );
            for s in &sessions {
                let last = s
                    .messages
                    .last()
                    .map(|m| format!("...{}", &m.content.chars().take(30).collect::<String>()))
                    .unwrap_or_default();
                let time = crate::ui::events::format_time(&s.updated_at);
                write_result(
                    ctx.renderer,
                    format!(
                        "  {}  {}  {}msgs  {}  {}",
                        &s.id[..8],
                        time,
                        s.messages.len(),
                        s.model,
                        last
                    ),
                );
            }
        }
    }
    Ok(())
}

pub fn rewind_target_items(session: &crate::session::Session) -> Vec<String> {
    session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(idx, msg)| {
            (msg.role == crate::session::MessageRole::User).then(|| {
                let preview: String = msg.content.chars().take(80).collect();
                format!("{}  {}", idx, preview.replace('\n', " "))
            })
        })
        .collect()
}

async fn handle_rewind(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    let target = if parts.len() >= 2 {
        match parts[1].parse::<usize>() {
            Ok(idx) if idx <= ctx.session.messages.len() => Some(idx),
            Ok(_) => {
                write_error(ctx.renderer, "message index out of range");
                return Ok(());
            }
            Err(_) => {
                write_error(ctx.renderer, "usage: /rewind <message-index>");
                return Ok(());
            }
        }
    } else {
        ctx.renderer
            .selected_session_message_indices()
            .into_iter()
            .find(|&idx| {
                ctx.session
                    .messages
                    .get(idx)
                    .is_some_and(|m| m.role == crate::session::MessageRole::User)
            })
    };

    let Some(message_index) = target else {
        write_ok(
            ctx.renderer,
            "usage: select a prior user message, then /rewind; or /rewind <index>",
        );
        for item in rewind_target_items(ctx.session) {
            write_result(ctx.renderer, format!("  {item}"));
        }
        return Ok(());
    };

    let text = ctx.session.messages[message_index].content.clone();
    ctx.session.rewind_to(message_index);
    if !ctx.cli.no_session {
        crate::session::storage::save_session(ctx.session)?;
    }
    render_session(ctx.renderer, ctx.session, ctx.cli, ctx.cfg, ctx.context)?;
    ctx.input.buffer = text.clone();
    ctx.input.cursor = text.len();
    write_ok(ctx.renderer, format!("rewound to message {message_index}"));
    Ok(())
}

async fn handle_redo(ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    if !ctx.session.redo() {
        write_ok(ctx.renderer, "nothing to redo");
        return Ok(());
    }
    if !ctx.cli.no_session {
        crate::session::storage::save_session(ctx.session)?;
    }
    render_session(ctx.renderer, ctx.session, ctx.cli, ctx.cfg, ctx.context)?;
    write_ok(ctx.renderer, "restored the last rewind");
    Ok(())
}

async fn handle_clear(ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    ctx.session.messages.clear();
    ctx.session.provider_calls.clear();
    ctx.session.total_estimated_tokens = 0;
    ctx.session.reset_calibration();
    ctx.session.compactions.clear();

    render_session(ctx.renderer, ctx.session, ctx.cli, ctx.cfg, ctx.context)?;
    Ok(())
}

async fn handle_undo(ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    let removed = undo_last(ctx.session);
    if removed == 0 {
        write_ok(ctx.renderer, "nothing to undo");
        return Ok(());
    }

    render_session(ctx.renderer, ctx.session, ctx.cli, ctx.cfg, ctx.context)?;
    write_ok(ctx.renderer, format!("removed {} message(s)", removed));

    write_ok(ctx.renderer, "  git stash working changes? [y/N] ");

    let mut buf = [0u8; 1];
    let do_stash =
        std::io::stdin().read_exact(&mut buf).is_ok() && (buf[0] == b'y' || buf[0] == b'Y');

    if do_stash {
        match std::process::Command::new("git").args(["stash"]).output() {
            Ok(out) if out.status.success() => {
                write_ok(ctx.renderer, "git stash done");
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                write_error(ctx.renderer, format!("git stash failed: {}", stderr.trim()));
            }
            Err(e) => {
                write_error(ctx.renderer, format!("git stash failed: {}", e));
            }
        }
    }

    Ok(())
}

async fn handle_retry(ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    let last_user = ctx
        .session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == crate::session::MessageRole::User)
        .cloned();
    match last_user {
        Some(msg) => {
            ctx.input.buffer = msg.content.clone();
            ctx.input.cursor = msg.content.len();
            write_ok(ctx.renderer, "edit last message and press Enter to retry");
        }
        None => {
            write_ok(ctx.renderer, "no previous message to retry");
        }
    }
    Ok(())
}

async fn handle_quit(ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    *ctx.is_running = false;
    Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "quit").into())
}

async fn handle_history(ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    match crate::session::chat_history::load_history() {
        Ok(entries) => {
            if entries.is_empty() {
                write_ok(ctx.renderer, "no chat history");
            } else {
                write_ok(
                    ctx.renderer,
                    format!("global chat history ({} entries):", entries.len()),
                );
                for entry in entries.iter().rev().take(10).rev() {
                    let preview: String = entry.content.chars().take(80).collect();
                    write_result(ctx.renderer, format!("  {}", preview));
                }
                if entries.len() > 10 {
                    write_ok(ctx.renderer, "  ... (showing last 10)");
                }
            }
        }
        Err(e) => {
            write_error(ctx.renderer, format!("failed to load chat history: {}", e));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::rewind_target_items;
    use crate::session::{MessageRole, Session};

    #[test]
    fn rewind_target_items_include_only_user_messages() {
        let mut session = Session::new("openai", "gpt-4", 128_000);
        session.add_message(MessageRole::System, "system");
        session.add_message(MessageRole::User, "first\nline");
        session.add_message(MessageRole::Assistant, "answer");
        session.add_message(MessageRole::User, "second");

        assert_eq!(
            rewind_target_items(&session),
            vec!["1  first line".to_string(), "3  second".to_string()]
        );
    }
}
