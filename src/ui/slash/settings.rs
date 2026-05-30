use crate::agent::tools;
use crate::config::types::EditSystem;
use crate::permission::SecurityMode;
use crate::ui::slash::{SlashCtx, write_error, write_ok, write_result};

pub async fn handle(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    match parts[0] {
        "/reasoning" | "/thinking" => handle_reasoning(parts, ctx).await,
        "/mode" => handle_mode(parts, ctx).await,
        "/toggle" => handle_toggle(parts, ctx).await,
        "/editsys" => handle_editsys(parts, ctx).await,
        #[cfg(feature = "mcp")]
        "/mcp" => handle_mcp(parts, ctx).await,
        #[cfg(not(feature = "mcp"))]
        "/mcp" => {
            write_error(
                ctx.renderer,
                "MCP support not enabled (build with --features mcp)",
            );
            Ok(())
        }
        _ => Ok(()),
    }
}

async fn handle_reasoning(_parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    *ctx.reasoning_enabled = !*ctx.reasoning_enabled;
    *ctx.show_reasoning = *ctx.reasoning_enabled;
    ctx.rebuild_agent().await;
    write_ok(
        ctx.renderer,
        format!(
            "reasoning: {}",
            if *ctx.reasoning_enabled { "on" } else { "off" }
        ),
    );
    Ok(())
}

async fn handle_mode(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    let current_mode = ctx
        .permission
        .as_ref()
        .map(|p| p.lock().unwrap_or_else(|e| e.into_inner()).mode())
        .unwrap_or(SecurityMode::Standard);

    if parts.len() < 2 {
        write_ok(ctx.renderer, "security mode:");
        write_result(ctx.renderer, format!("  current: {}", current_mode));
        write_result(ctx.renderer, "");
        write_result(
            ctx.renderer,
            "  /mode standard      allow within CWD, ask for external",
        );
        write_result(ctx.renderer, "  /mode restrictive   ask for all operations");
        write_result(
            ctx.renderer,
            "  /mode readonly      allow reads, deny everything else",
        );
        write_result(
            ctx.renderer,
            "  /mode guarded       allow reads, ask for everything else",
        );
        write_result(
            ctx.renderer,
            "  /mode yolo          allow all, ask for destructive bash",
        );
        return Ok(());
    }
    match parts[1] {
        "standard" => set_mode(ctx, SecurityMode::Standard, "standard").await,
        "restrictive" => set_mode(ctx, SecurityMode::Restrictive, "restrictive").await,
        "readonly" => set_mode(ctx, SecurityMode::ReadOnly, "readonly").await,
        "guarded" => set_mode(ctx, SecurityMode::Guarded, "guarded").await,
        "yolo" => set_mode(ctx, SecurityMode::Yolo, "yolo").await,
        _ => write_error(ctx.renderer, format!("unknown mode: {}", parts[1])),
    }
    Ok(())
}

async fn set_mode(ctx: &mut SlashCtx<'_>, mode: SecurityMode, label: &str) {
    if let Some(p) = ctx.permission {
        p.lock().unwrap_or_else(|e| e.into_inner()).set_mode(mode);
        write_ok(ctx.renderer, format!("security mode: {}", label));
    } else {
        write_error(ctx.renderer, "permission system not active");
    }
}

async fn handle_toggle(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    if parts.len() < 2 {
        write_ok(ctx.renderer, "usage: /toggle <feature> [on|off]");
        write_ok(ctx.renderer, "features:");
        write_result(
            ctx.renderer,
            format!(
                "  todo  {}",
                if *ctx.todo_tools_enabled { "on" } else { "off" }
            ),
        );
    } else {
        let new_state = match parts.get(2).copied() {
            Some("on") => true,
            Some("off") => false,
            Some(other) => {
                write_error(ctx.renderer, format!("invalid: '{}', use on or off", other));
                return Ok(());
            }
            None => !*ctx.todo_tools_enabled,
        };
        if new_state == *ctx.todo_tools_enabled {
            write_ok(
                ctx.renderer,
                format!(
                    "todo tools already {}",
                    if new_state { "on" } else { "off" }
                ),
            );
        } else {
            *ctx.todo_tools_enabled = new_state;
            ctx.rebuild_agent().await;
            write_ok(
                ctx.renderer,
                format!(
                    "todo tools: {}",
                    if *ctx.todo_tools_enabled { "on" } else { "off" }
                ),
            );
        }
    }
    Ok(())
}

async fn handle_editsys(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    let current = tools::edit_system();
    if parts.len() < 2 {
        write_ok(ctx.renderer, format!("edit system: {}", current));
        write_result(
            ctx.renderer,
            "  /editsys similarity   SEARCH/REPLACE with fuzzy matching",
        );
        write_result(
            ctx.renderer,
            "  /editsys hashedit     tag-based (CRC-32 line hashes)",
        );
        return Ok(());
    }
    match parts[1] {
        "similarity" => {
            tools::set_edit_system(EditSystem::Similarity);
            write_ok(ctx.renderer, "edit system: similarity (SEARCH/REPLACE)");
        }
        "hashedit" => {
            tools::set_edit_system(EditSystem::Hashedit);
            write_ok(ctx.renderer, "edit system: hashedit (tag-based)");
        }
        _ => write_error(
            ctx.renderer,
            format!("unknown: '{}' (similarity|hashedit)", parts[1]),
        ),
    }
    Ok(())
}

#[cfg(feature = "mcp")]
async fn handle_mcp(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    let Some(mgr) = ctx.mcp_manager else {
        write_ok(ctx.renderer, "no MCP servers configured");
        return Ok(());
    };
    if mgr.handles.is_empty() {
        write_ok(ctx.renderer, "no MCP servers connected");
    } else if parts.len() == 1 {
        write_ok(ctx.renderer, "MCP servers:");
        for handle in &mgr.handles {
            match handle.list_tools().await {
                Ok(tools) => {
                    write_result(
                        ctx.renderer,
                        format!("  {} ({} tools)", handle.server_name, tools.len()),
                    );
                }
                Err(e) => {
                    write_error(
                        ctx.renderer,
                        format!("  {} (error: {})", handle.server_name, e),
                    );
                }
            }
        }
    } else {
        let name = parts[1].trim();
        if let Some(handle) = mgr.handles.iter().find(|h| h.server_name == name) {
            match handle.list_tools().await {
                Ok(tools) => {
                    if tools.is_empty() {
                        write_ok(ctx.renderer, format!("server '{}' has no tools", name));
                    } else {
                        write_ok(ctx.renderer, format!("tools on '{}':", name));
                        for tool in &tools {
                            let desc = tool.description.as_deref().unwrap_or("");
                            write_result(ctx.renderer, format!("  {}  {}", tool.name, desc));
                        }
                    }
                }
                Err(e) => {
                    write_error(
                        ctx.renderer,
                        format!("error listing tools on '{}': {}", name, e),
                    );
                }
            }
        } else {
            write_error(ctx.renderer, format!("unknown MCP server: '{}'", name));
        }
    }
    Ok(())
}
