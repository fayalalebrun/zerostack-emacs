use crate::context;
use crate::session::storage;
use crate::ui::slash::{SlashCtx, write_error, write_ok, write_result};

pub async fn handle(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    match parts[0] {
        "/theme" => handle_theme(parts, ctx).await,
        "/regen-themes" => handle_regen_themes(ctx).await,
        _ => Ok(()),
    }
}

async fn handle_theme(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    let mut sorted: Vec<&String> = ctx.context.themes.keys().collect();
    sorted.sort();
    if parts.len() < 2 {
        if sorted.is_empty() {
            write_ok(ctx.renderer, "no themes available");
        } else {
            let current = ctx
                .context
                .current_theme_name
                .as_deref()
                .unwrap_or("(none)");
            write_ok(
                ctx.renderer,
                format!("available themes (current: {}):", current),
            );
            for name in &sorted {
                write_result(ctx.renderer, format!("  {}", name));
            }
            write_result(ctx.renderer, "usage: /theme <name>  |  /theme default");
        }
    } else if parts[1] == "default" {
        if ctx.context.current_theme_name.is_none() {
            write_ok(ctx.renderer, "no active theme to clear");
        } else {
            ctx.context.current_theme_name = None;
            let _ = storage::save_theme_name(None);
            if let Some(colors) = &ctx.cfg.colors {
                let chat_bg = colors
                    .chat_background
                    .as_deref()
                    .and_then(crate::ui::utils::parse_color);
                let input_bg = colors
                    .input_background
                    .as_deref()
                    .and_then(crate::ui::utils::parse_color);
                let status_bg = colors
                    .status_background
                    .as_deref()
                    .and_then(crate::ui::utils::parse_color);
                ctx.renderer
                    .set_background_colors(chat_bg, input_bg, status_bg);
            }
            write_ok(ctx.renderer, "theme cleared (using config colors)");
        }
    } else {
        let name = parts[1].trim();
        if let Some(content) = ctx.context.themes.get(name) {
            ctx.context.current_theme_name = Some(name.to_string());
            let _ = storage::save_theme_name(Some(name));
            crate::context::themes::apply(content, ctx.renderer);
            write_ok(ctx.renderer, format!("active theme: {}", name));
        } else {
            write_error(ctx.renderer, format!("unknown theme: '{}'", name));
            if !sorted.is_empty() {
                write_ok(ctx.renderer, "available themes:");
                for t in &sorted {
                    write_result(ctx.renderer, format!("  {}", t));
                }
            }
        }
    }
    Ok(())
}

async fn handle_regen_themes(ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    match context::themes::regen() {
        Ok(()) => {
            ctx.context.themes = context::themes::load();
            write_ok(ctx.renderer, "default themes regenerated");
        }
        Err(e) => {
            write_error(ctx.renderer, format!("failed to regenerate themes: {}", e));
        }
    }
    Ok(())
}
