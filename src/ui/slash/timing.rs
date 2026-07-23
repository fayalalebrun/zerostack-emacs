use crate::ui::slash::{SlashCtx, write_result};

pub async fn handle(ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    for line in crate::session::timing::format_report(ctx.session).lines() {
        write_result(ctx.renderer, line);
    }
    Ok(())
}
