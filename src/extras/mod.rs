#[cfg(feature = "loop")]
pub mod r#loop;

#[cfg(feature = "git-worktree")]
pub mod git_worktree;

#[cfg(all(unix, feature = "git-worktree"))]
pub mod workspace_jobs;

#[cfg(feature = "mcp")]
pub mod mcp;

#[cfg(feature = "acp")]
pub mod acp;

#[cfg(feature = "memory")]
pub mod memory;

#[cfg(feature = "subagents")]
pub mod subagents;

#[cfg(feature = "archmd")]
pub mod archmd;

#[cfg(feature = "advisor")]
pub mod advisor;

pub mod emacs;

pub mod emacs_attention;

pub mod emacs_board;

#[cfg(unix)]
pub mod session_cli;

#[cfg(feature = "veles")]
pub mod session_search;

#[cfg(feature = "multimodal")]
pub mod image_validate;

#[cfg(feature = "multimodal")]
pub mod multimodal;

pub mod status_signals;

pub(crate) mod truncate;
