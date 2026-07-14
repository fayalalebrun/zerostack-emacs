use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::config::{self, Config};
use crate::session::{Session, storage};

#[derive(Debug, Clone)]
struct BoardProject {
    name: String,
    path: PathBuf,
    repo: PathBuf,
    alive: bool,
    updated_at: String,
    worktrees: Vec<BoardWorktree>,
}

#[derive(Debug, Clone)]
struct BoardSnapshot {
    provider: String,
    model: String,
    subagent_provider: String,
    subagent_model: String,
    needs_attention: Vec<BoardSession>,
    projects: Vec<BoardProject>,
    loose_workspaces: Vec<BoardLooseWorkspace>,
}

#[derive(Debug, Clone)]
struct BoardLooseWorkspace {
    path: PathBuf,
    alive: bool,
    updated_at: String,
    sessions: Vec<BoardSession>,
}

#[derive(Debug, Clone)]
struct BoardWorktree {
    path: PathBuf,
    branch: String,
    description: String,
    alive: bool,
    sessions: Vec<BoardSession>,
}

#[derive(Debug, Clone)]
struct BoardSession {
    id: String,
    title: String,
    cwd: String,
    model: String,
    provider: String,
    created_at: String,
    updated_at: String,
    message_count: usize,
    tokens: u64,
    context_window: u64,
    cost: f64,
    alive: bool,
    pid: Option<u32>,
    socket: Option<String>,
}

#[derive(Debug, Clone)]
struct GitSessionInfo {
    repo: PathBuf,
    project_path: PathBuf,
    worktree: PathBuf,
}

#[derive(Debug, Clone)]
struct GitWorktree {
    path: PathBuf,
    branch: String,
    detached: bool,
}

#[derive(Debug, Clone)]
struct ProjectBuilder {
    repo: PathBuf,
    project_path: PathBuf,
    sessions: HashMap<PathBuf, Vec<BoardSession>>,
    anchors: Vec<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
struct LiveSessionMeta {
    session_id: String,
    pid: u32,
    socket: String,
    updated_at: Option<String>,
}

pub fn print_board() -> anyhow::Result<()> {
    let snapshot = collect_board()?;
    println!("{}", board_to_sexp(&snapshot));
    Ok(())
}

fn collect_board() -> anyhow::Result<BoardSnapshot> {
    let (cfg, _) = config::load();
    let (provider, model, subagent_provider, subagent_model) = board_defaults(&cfg);
    let sessions = storage::find_all_sessions()?;
    let live = live_sessions_by_id()?;
    let attention = crate::extras::emacs_attention::list()?;
    let mut needs_attention = Vec::new();
    let mut projects: HashMap<PathBuf, ProjectBuilder> = HashMap::new();
    let mut loose: HashMap<PathBuf, Vec<BoardSession>> = HashMap::new();

    for session in sessions {
        let dir = Path::new(session.working_dir.as_str());
        if !session_directory_exists(dir) {
            continue;
        }
        let live_meta = live.get(session.id.as_str());
        let board_session = board_session(&session, live_meta);
        if attention.contains(session.id.as_str()) {
            needs_attention.push(board_session.clone());
        }
        let Some(git) = git_session_info(dir) else {
            loose
                .entry(workspace_path(dir))
                .or_default()
                .push(board_session);
            continue;
        };
        let builder = projects
            .entry(git.repo.clone())
            .or_insert_with(|| ProjectBuilder {
                repo: git.repo.clone(),
                project_path: git.project_path.clone(),
                sessions: HashMap::new(),
                anchors: Vec::new(),
            });
        builder.anchors.push(git.worktree.clone());
        builder
            .sessions
            .entry(git.worktree)
            .or_default()
            .push(board_session);
    }

    let mut projects = projects
        .into_values()
        .map(project_from_builder)
        .collect::<Vec<_>>();
    sort_projects(&mut projects);
    let mut loose_workspaces = loose
        .into_iter()
        .map(|(path, mut sessions)| {
            sort_sessions(&mut sessions);
            let alive = sessions.iter().any(|session| session.alive);
            let updated_at = sessions
                .iter()
                .map(|session| session.updated_at.as_str())
                .max()
                .unwrap_or_default()
                .to_string();
            BoardLooseWorkspace {
                path,
                alive,
                updated_at,
                sessions,
            }
        })
        .collect::<Vec<_>>();
    sort_loose_workspaces(&mut loose_workspaces);
    sort_sessions(&mut needs_attention);
    Ok(BoardSnapshot {
        provider,
        model,
        subagent_provider,
        subagent_model,
        needs_attention,
        projects,
        loose_workspaces,
    })
}

fn board_defaults(cfg: &Config) -> (String, String, String, String) {
    let provider = config::commands::default_provider_name(cfg);
    let model = cfg
        .model
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| {
            crate::provider::default_model_for_provider(&provider, cfg).map(|(model, _)| model)
        })
        .unwrap_or_else(|| "model".to_string());
    #[cfg(feature = "subagents")]
    let subagent_provider = cfg
        .subagent_provider
        .as_ref()
        .map(|provider| config::commands::canonical_provider_name(provider))
        .unwrap_or_else(|| provider.clone());
    #[cfg(not(feature = "subagents"))]
    let subagent_provider = provider.clone();
    #[cfg(feature = "subagents")]
    let subagent_model = cfg
        .subagent_model
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| {
            crate::provider::default_model_for_provider(&subagent_provider, cfg)
                .map(|(model, _)| model)
        })
        .unwrap_or_else(|| model.clone());
    #[cfg(not(feature = "subagents"))]
    let subagent_model = model.clone();

    (provider, model, subagent_provider, subagent_model)
}

fn session_directory_exists(dir: &Path) -> bool {
    dir.is_dir()
}

fn workspace_path(dir: &Path) -> PathBuf {
    canonicalize_existing(dir).unwrap_or_else(|| dir.to_path_buf())
}

fn project_from_builder(builder: ProjectBuilder) -> BoardProject {
    let mut worktrees = builder
        .anchors
        .first()
        .and_then(|anchor| list_git_worktrees(anchor).ok())
        .filter(|worktrees| !worktrees.is_empty())
        .unwrap_or_else(|| {
            builder
                .sessions
                .keys()
                .map(|path| GitWorktree {
                    path: path.clone(),
                    branch: current_branch(path).unwrap_or_default(),
                    detached: false,
                })
                .collect()
        });

    for session_path in builder.sessions.keys() {
        if !worktrees
            .iter()
            .any(|worktree| worktree.path == *session_path)
            && session_path.exists()
        {
            worktrees.push(GitWorktree {
                path: session_path.clone(),
                branch: current_branch(session_path).unwrap_or_default(),
                detached: false,
            });
        }
    }

    let mut worktrees = worktrees
        .into_iter()
        .filter(|worktree| worktree.path.exists())
        .map(|worktree| {
            let mut sessions = builder
                .sessions
                .get(&worktree.path)
                .cloned()
                .unwrap_or_default();
            sort_sessions(&mut sessions);
            let branch = if worktree.detached && worktree.branch.is_empty() {
                "detached".to_string()
            } else {
                worktree.branch
            };
            let description = if branch.is_empty() || branch == "detached" {
                String::new()
            } else {
                branch_description(&worktree.path, &branch).unwrap_or_default()
            };
            let alive = sessions.iter().any(|session| session.alive);
            BoardWorktree {
                path: worktree.path,
                branch,
                description,
                alive,
                sessions,
            }
        })
        .collect::<Vec<_>>();
    sort_worktrees(&mut worktrees);

    let alive = worktrees.iter().any(|worktree| worktree.alive);
    let updated_at = worktrees
        .iter()
        .flat_map(|worktree| worktree.sessions.iter())
        .map(|session| session.updated_at.as_str())
        .max()
        .unwrap_or_default()
        .to_string();

    BoardProject {
        name: project_name(&builder.project_path),
        path: builder.project_path,
        repo: builder.repo,
        alive,
        updated_at,
        worktrees,
    }
}

fn board_session(session: &Session, live: Option<&LiveSessionMeta>) -> BoardSession {
    BoardSession {
        id: session.id.to_string(),
        title: session.title(),
        cwd: session.working_dir.to_string(),
        model: session.model.to_string(),
        provider: session.provider.to_string(),
        created_at: session.created_at.to_string(),
        updated_at: live
            .and_then(|meta| meta.updated_at.clone())
            .unwrap_or_else(|| session.updated_at.to_string()),
        message_count: session.messages.len(),
        tokens: session.effective_context_tokens(),
        context_window: session.context_window,
        cost: session.total_cost,
        alive: live.is_some(),
        pid: live.map(|meta| meta.pid),
        socket: live.map(|meta| meta.socket.clone()),
    }
}

fn git_session_info(dir: &Path) -> Option<GitSessionInfo> {
    if !dir.exists() {
        return None;
    }
    let worktree = git_output(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    let worktree = resolve_git_path(dir, worktree.trim()).ok()?;
    let common = git_output(&worktree, &["rev-parse", "--git-common-dir"]).ok()?;
    let repo = resolve_git_path(&worktree, common.trim()).ok()?;
    let project_path = project_path_from_repo(&repo);
    Some(GitSessionInfo {
        repo,
        project_path,
        worktree,
    })
}

fn list_git_worktrees(anchor: &Path) -> anyhow::Result<Vec<GitWorktree>> {
    let output = git_output(anchor, &["worktree", "list", "--porcelain"])?;
    let mut worktrees = parse_worktree_porcelain(&output)
        .into_iter()
        .filter_map(|mut worktree| {
            worktree.path = canonicalize_existing(&worktree.path)?;
            Some(worktree)
        })
        .collect::<Vec<_>>();
    worktrees.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(worktrees)
}

fn parse_worktree_porcelain(output: &str) -> Vec<GitWorktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<GitWorktree> = None;

    for line in output.lines() {
        if line.is_empty() {
            push_worktree(&mut worktrees, &mut current);
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            push_worktree(&mut worktrees, &mut current);
            current = Some(GitWorktree {
                path: PathBuf::from(path),
                branch: String::new(),
                detached: false,
            });
        } else if let Some(branch) = line.strip_prefix("branch ") {
            if let Some(worktree) = &mut current {
                worktree.branch = branch_name(branch);
            }
        } else if line == "detached" {
            if let Some(worktree) = &mut current {
                worktree.detached = true;
            }
        }
    }
    push_worktree(&mut worktrees, &mut current);
    worktrees
}

fn push_worktree(worktrees: &mut Vec<GitWorktree>, current: &mut Option<GitWorktree>) {
    if let Some(worktree) = current.take() {
        worktrees.push(worktree);
    }
}

fn git_output(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git in {}", dir.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed in {}: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn current_branch(path: &Path) -> Option<String> {
    let output = git_output(path, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    let branch = output.trim();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch.to_string())
    }
}

fn branch_description(path: &Path, branch: &str) -> Option<String> {
    let key = format!("branch.{}.description", branch);
    let output = git_output(path, &["config", "--get", &key]).ok()?;
    let description = output.trim();
    if description.is_empty() {
        None
    } else {
        Some(description.to_string())
    }
}

fn resolve_git_path(base: &Path, value: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    path.canonicalize()
        .with_context(|| format!("failed to resolve git path {}", path.display()))
}

fn canonicalize_existing(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

fn project_path_from_repo(repo: &Path) -> PathBuf {
    if repo.file_name().is_some_and(|name| name == ".git")
        && let Some(parent) = repo.parent()
    {
        return parent.to_path_buf();
    }
    repo.to_path_buf()
}

fn branch_name(refname: &str) -> String {
    refname
        .strip_prefix("refs/heads/")
        .unwrap_or(refname)
        .to_string()
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn sort_projects(projects: &mut [BoardProject]) {
    for project in projects.iter_mut() {
        sort_worktrees(&mut project.worktrees);
        project.alive = project.worktrees.iter().any(|worktree| worktree.alive);
    }
    projects.sort_by(|a, b| {
        b.alive
            .cmp(&a.alive)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn sort_worktrees(worktrees: &mut [BoardWorktree]) {
    for worktree in worktrees.iter_mut() {
        sort_sessions(&mut worktree.sessions);
        worktree.alive = worktree.sessions.iter().any(|session| session.alive);
    }
    worktrees.sort_by(|a, b| {
        b.alive
            .cmp(&a.alive)
            .then_with(|| b.sessions.len().cmp(&a.sessions.len()))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn sort_sessions(sessions: &mut [BoardSession]) {
    sessions.sort_by(|a, b| {
        b.alive
            .cmp(&a.alive)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn sort_loose_workspaces(workspaces: &mut [BoardLooseWorkspace]) {
    for workspace in workspaces.iter_mut() {
        sort_sessions(&mut workspace.sessions);
        workspace.alive = workspace.sessions.iter().any(|session| session.alive);
    }
    workspaces.sort_by(|a, b| {
        b.alive
            .cmp(&a.alive)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn live_sessions_by_id() -> anyhow::Result<HashMap<String, LiveSessionMeta>> {
    Ok(list_live_session_metas()?
        .into_iter()
        .map(|meta| (meta.session_id.clone(), meta))
        .collect())
}

#[cfg(unix)]
fn list_live_session_metas() -> anyhow::Result<Vec<LiveSessionMeta>> {
    let root = sessions_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut metas = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let socket = path.join("sock");
        if !socket_alive(&socket) {
            let _ = std::fs::remove_dir_all(&path);
            continue;
        }
        let Ok(json) = std::fs::read_to_string(path.join("meta.json")) else {
            continue;
        };
        if let Ok(meta) = serde_json::from_str::<LiveSessionMeta>(&json) {
            metas.push(meta);
        }
    }
    Ok(metas)
}

#[cfg(not(unix))]
fn list_live_session_metas() -> anyhow::Result<Vec<LiveSessionMeta>> {
    Ok(Vec::new())
}

#[cfg(unix)]
fn runtime_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZS_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("zerostack");
    }
    storage::data_dir().join("runtime")
}

#[cfg(unix)]
fn sessions_root() -> PathBuf {
    runtime_root().join("sessions")
}

#[cfg(unix)]
fn socket_alive(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

fn board_to_sexp(snapshot: &BoardSnapshot) -> String {
    format!(
        "(zerostack-board :version 1 :provider {} :model {} :subagent-provider {} :subagent-model {} :needs-attention ({}) :projects ({}) :loose-workspaces ({}))",
        sexp_quote(&snapshot.provider),
        sexp_quote(&snapshot.model),
        sexp_quote(&snapshot.subagent_provider),
        sexp_quote(&snapshot.subagent_model),
        snapshot
            .needs_attention
            .iter()
            .map(session_to_sexp)
            .collect::<Vec<_>>()
            .join(" "),
        snapshot
            .projects
            .iter()
            .map(project_to_sexp)
            .collect::<Vec<_>>()
            .join(" "),
        snapshot
            .loose_workspaces
            .iter()
            .map(loose_workspace_to_sexp)
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn loose_workspace_to_sexp(workspace: &BoardLooseWorkspace) -> String {
    format!(
        "(:path {} :alive {} :updated-at {} :sessions ({}))",
        sexp_quote_path(&workspace.path),
        sexp_bool(workspace.alive),
        sexp_quote(&workspace.updated_at),
        workspace
            .sessions
            .iter()
            .map(session_to_sexp)
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn project_to_sexp(project: &BoardProject) -> String {
    format!(
        "(:name {} :path {} :repo {} :alive {} :updated-at {} :worktrees ({}))",
        sexp_quote(&project.name),
        sexp_quote_path(&project.path),
        sexp_quote_path(&project.repo),
        sexp_bool(project.alive),
        sexp_quote(&project.updated_at),
        project
            .worktrees
            .iter()
            .map(worktree_to_sexp)
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn worktree_to_sexp(worktree: &BoardWorktree) -> String {
    format!(
        "(:path {} :branch {} :description {} :alive {} :sessions ({}))",
        sexp_quote_path(&worktree.path),
        sexp_quote(&worktree.branch),
        sexp_quote(&worktree.description),
        sexp_bool(worktree.alive),
        worktree
            .sessions
            .iter()
            .map(session_to_sexp)
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn session_to_sexp(session: &BoardSession) -> String {
    format!(
        "(:id {} :short-id {} :title {} :cwd {} :model {} :provider {} :created-at {} :updated-at {} :message-count {} :tokens {} :context-window {} :cost {:.6} :alive {} :pid {} :socket {})",
        sexp_quote(&session.id),
        sexp_quote(short_id(&session.id)),
        sexp_quote(&session.title),
        sexp_quote(&session.cwd),
        sexp_quote(&session.model),
        sexp_quote(&session.provider),
        sexp_quote(&session.created_at),
        sexp_quote(&session.updated_at),
        session.message_count,
        session.tokens,
        session.context_window,
        session.cost,
        sexp_bool(session.alive),
        session
            .pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "nil".to_string()),
        session
            .socket
            .as_deref()
            .map(sexp_quote)
            .unwrap_or_else(|| "nil".to_string()),
    )
}

fn sexp_quote_path(path: &Path) -> String {
    sexp_quote(&path.to_string_lossy())
}

fn sexp_bool(value: bool) -> &'static str {
    if value { "t" } else { "nil" }
}

fn sexp_quote(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('"');
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, updated_at: &str, alive: bool) -> BoardSession {
        BoardSession {
            id: id.to_string(),
            title: format!("session {id}"),
            cwd: "/repo".to_string(),
            model: "model".to_string(),
            provider: "provider".to_string(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            message_count: 1,
            tokens: 10,
            context_window: 100,
            cost: 0.0,
            alive,
            pid: alive.then_some(123),
            socket: alive.then_some("/tmp/sock".to_string()),
        }
    }

    #[test]
    fn session_directory_must_exist() {
        let root = std::env::temp_dir().join(format!(
            "zs-emacs-board-directory-test-{}",
            std::process::id()
        ));
        let file = root.with_extension("file");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&file);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&file, "not a directory").unwrap();

        assert!(session_directory_exists(&root));
        assert!(!session_directory_exists(&file));
        std::fs::remove_dir_all(&root).unwrap();
        assert!(!session_directory_exists(&root));
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn parses_git_worktree_porcelain() {
        let worktrees = parse_worktree_porcelain(
            "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /repo-wt\nHEAD def\ndetached\n\n",
        );
        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].path, PathBuf::from("/repo"));
        assert_eq!(worktrees[0].branch, "main");
        assert!(worktrees[1].detached);
    }

    #[test]
    fn orders_projects_worktrees_and_sessions_by_alive_children() {
        let mut projects = vec![
            BoardProject {
                name: "dead".to_string(),
                path: PathBuf::from("/dead"),
                repo: PathBuf::from("/dead/.git"),
                alive: false,
                updated_at: "2026-01-02".to_string(),
                worktrees: vec![BoardWorktree {
                    path: PathBuf::from("/dead"),
                    branch: "main".to_string(),
                    description: String::new(),
                    alive: false,
                    sessions: vec![session("dead-session", "2026-01-02", false)],
                }],
            },
            BoardProject {
                name: "live".to_string(),
                path: PathBuf::from("/live"),
                repo: PathBuf::from("/live/.git"),
                alive: false,
                updated_at: "2026-01-01".to_string(),
                worktrees: vec![
                    BoardWorktree {
                        path: PathBuf::from("/live-dead"),
                        branch: "dead".to_string(),
                        description: String::new(),
                        alive: false,
                        sessions: vec![session("old", "2026-01-03", false)],
                    },
                    BoardWorktree {
                        path: PathBuf::from("/live"),
                        branch: "main".to_string(),
                        description: String::new(),
                        alive: false,
                        sessions: vec![
                            session("z-dead", "2026-01-04", false),
                            session("a-live", "2026-01-01", true),
                        ],
                    },
                ],
            },
        ];

        sort_projects(&mut projects);
        assert_eq!(projects[0].name, "live");
        assert!(projects[0].alive);
        assert_eq!(projects[0].worktrees[0].path, PathBuf::from("/live"));
        assert!(projects[0].worktrees[0].alive);
        assert_eq!(projects[0].worktrees[0].sessions[0].id, "a-live");
    }

    #[test]
    fn orders_loose_workspaces_by_alive_then_recent() {
        let mut workspaces = vec![
            BoardLooseWorkspace {
                path: PathBuf::from("/old-live"),
                alive: false,
                updated_at: "2026-01-01".to_string(),
                sessions: vec![session("live", "2026-01-01", true)],
            },
            BoardLooseWorkspace {
                path: PathBuf::from("/new-dead"),
                alive: false,
                updated_at: "2026-01-05".to_string(),
                sessions: vec![session("dead", "2026-01-05", false)],
            },
        ];

        sort_loose_workspaces(&mut workspaces);
        assert_eq!(workspaces[0].path, PathBuf::from("/old-live"));
        assert!(workspaces[0].alive);
        assert_eq!(workspaces[1].path, PathBuf::from("/new-dead"));
    }

    #[test]
    fn board_sexp_is_easy_for_emacs_to_read() {
        let projects = vec![BoardProject {
            name: "repo".to_string(),
            path: PathBuf::from("/repo"),
            repo: PathBuf::from("/repo/.git"),
            alive: true,
            updated_at: "2026-06-20T00:00:00Z".to_string(),
            worktrees: vec![BoardWorktree {
                path: PathBuf::from("/repo"),
                branch: "main".to_string(),
                description: "branch \"description\"".to_string(),
                alive: true,
                sessions: vec![session("123456789", "2026-06-20T00:00:00Z", true)],
            }],
        }];

        let snapshot = BoardSnapshot {
            provider: "openai-codex".to_string(),
            model: "gpt-5.5".to_string(),
            subagent_provider: "openrouter".to_string(),
            subagent_model: "deepseek/deepseek-chat-v3.1".to_string(),
            needs_attention: vec![session("attention-session", "2026-06-21T00:00:00Z", true)],
            projects,
            loose_workspaces: vec![BoardLooseWorkspace {
                path: PathBuf::from("/nongit"),
                alive: false,
                updated_at: "2026-06-19T00:00:00Z".to_string(),
                sessions: vec![session("loose-session", "2026-06-19T00:00:00Z", false)],
            }],
        };

        let sexp = board_to_sexp(&snapshot);
        assert!(sexp.starts_with("(zerostack-board :version 1 :provider \"openai-codex\""));
        assert!(sexp.contains(":model \"gpt-5.5\""));
        assert!(sexp.contains(":subagent-provider \"openrouter\""));
        assert!(sexp.contains(":subagent-model \"deepseek/deepseek-chat-v3.1\""));
        assert!(sexp.contains(":needs-attention"));
        assert!(sexp.contains(":id \"attention-session\""));
        assert!(sexp.contains(":loose-workspaces"));
        assert!(sexp.contains(":path \"/nongit\""));
        assert!(sexp.contains(":description \"branch \\\"description\\\"\""));
        assert!(sexp.contains(":short-id \"12345678\""));
        assert!(sexp.contains(":alive t"));
        assert!(sexp.contains(":socket \"/tmp/sock\""));
    }
}
