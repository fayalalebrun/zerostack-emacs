use crate::session::MessageRole;
use crate::ui::slash::{SlashCtx, write_ok};

fn is_session_empty(ctx: &SlashCtx<'_>) -> bool {
    !ctx.session
        .messages
        .iter()
        .any(|m| m.role == MessageRole::User)
}

fn is_in_worktree() -> bool {
    #[cfg(feature = "git-worktree")]
    {
        crate::extras::git_worktree::detect().is_some()
    }
    #[cfg(not(feature = "git-worktree"))]
    {
        false
    }
}

fn build_default_review_message(session_empty: bool, in_worktree: bool) -> String {
    match (session_empty, in_worktree) {
        (true, true) => "Review the current worktree state. Check the diff from the base branch \
                         for correctness, design, testing, and security."
            .to_string(),
        (true, false) => "Review the current codebase for correctness, design, testing, and \
                          security."
            .to_string(),
        (false, true) => "Review the changes in this worktree session. Consider the diff from \
                          main since the branch was created. Check for correctness, design, \
                          testing, and security."
            .to_string(),
        (false, false) => "Review the changes discussed in this session for correctness, \
                           design, testing, and security."
            .to_string(),
    }
}

pub async fn handle(parts: &[&str], ctx: &mut SlashCtx<'_>) -> anyhow::Result<()> {
    let msg = if parts.len() > 1 {
        parts[1..].join(" ")
    } else {
        let session_empty = is_session_empty(ctx);
        let in_worktree = is_in_worktree();
        build_default_review_message(session_empty, in_worktree)
    };

    write_ok(ctx.renderer, format!("review: {}", msg));

    Err(anyhow::anyhow!("DEFER_REVIEW:{}", msg))
}
