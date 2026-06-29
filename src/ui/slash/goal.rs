use crate::agent::tools::goal::GOAL;
use crate::ui::slash::{SlashCtx, write_ok, write_result};

pub async fn handle(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    if parts.get(1).is_some_and(|arg| *arg == "clear") {
        crate::agent::tools::goal::clear_goal_state();
        ctx.session.goal = None;
        if !ctx.cli.no_session {
            crate::session::storage::save_session(ctx.session)?;
        }
        write_ok(ctx.renderer, "goal cleared");
        return Ok(());
    }

    let goal = GOAL.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(goal) = goal else {
        write_result(ctx.renderer, "no goal");
        return Ok(());
    };

    let icon = match goal.status.as_str() {
        "completed" => "[x]",
        "in_progress" => "[>]",
        "cancelled" => "[-]",
        _ => "[ ]",
    };
    let verdict = goal
        .evaluator_status
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!(" [{}]", s.trim()))
        .unwrap_or_default();
    write_ok(ctx.renderer, "goal:");
    write_result(
        ctx.renderer,
        format!("  {} [{}] {}{}", icon, goal.priority, goal.content, verdict),
    );
    if let Some(evidence) = goal.evidence.as_deref().filter(|s| !s.trim().is_empty()) {
        write_result(ctx.renderer, format!("      evidence: {}", evidence.trim()));
    }
    Ok(())
}
