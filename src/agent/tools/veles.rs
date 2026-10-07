use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use rig::tool::Tool;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use veles_core::{SearchMode, SearchResult, VelesIndex};

use crate::agent::tools::{
    AskSender, PermCheck, ToolError, check_perm, check_perm_path, truncate_live_tool_output,
};

#[derive(Deserialize)]
pub struct VelesArgs {
    pub query: String,
    pub top_k: Option<usize>,
    pub mode: Option<String>,
    pub language: Option<String>,
    pub path: Option<String>,
}

pub struct VelesTool {
    permission: Option<PermCheck>,
    ask_tx: Option<AskSender>,
}

impl VelesTool {
    pub fn new(permission: Option<PermCheck>, ask_tx: Option<AskSender>) -> Self {
        Self { permission, ask_tx }
    }
}

impl Tool for VelesTool {
    const NAME: &'static str = "code_search";

    type Error = ToolError;
    type Args = VelesArgs;
    type Output = String;

    fn description(&self) -> String {
        "Search the current repository by behavior, intent, architecture, identifier, or code fragment. Use this first when you do not know the exact file or symbol; use grep instead for exact regex matches or exhaustive occurrence lists. The persistent Veles index is refreshed automatically and shared by all Git worktrees without writing into the repository.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language description, identifier, or code fragment to find"
                },
                "top_k": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "description": "Maximum results (default 5)"
                },
                "mode": {
                    "type": "string",
                    "enum": ["hybrid", "semantic", "bm25"],
                    "description": "Search mode (default hybrid)"
                },
                "language": {
                    "type": "string",
                    "description": "Optional language filter such as rust, python, or typescript"
                },
                "path": {
                    "type": "string",
                    "description": "Optional file or directory to search within, relative to the working directory or absolute"
                }
            },
            "required": ["query"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::tool::ToolContext,
        args: VelesArgs,
    ) -> Result<String, ToolError> {
        let query = args.query.trim().to_string();
        if query.is_empty() {
            return Err(ToolError::Msg("query must not be empty".to_string()));
        }
        let coaching = match args.path.as_deref() {
            Some(path) => check_perm_path(&self.permission, &self.ask_tx, Self::NAME, path).await?,
            None => check_perm(&self.permission, &self.ask_tx, Self::NAME, &query).await?,
        };
        let top_k = args.top_k.unwrap_or(5).clamp(1, 20);
        let mode = SearchMode::from_str(args.mode.as_deref().unwrap_or("hybrid"))
            .map_err(ToolError::Msg)?;
        let languages = args.language.map(|language| vec![language]);
        let path = args.path;

        let output = tokio::task::spawn_blocking(move || {
            let cwd = std::env::current_dir()?;
            search_scoped(
                &cwd,
                path.as_deref(),
                &cache_base_dir(),
                &query,
                top_k,
                mode,
                languages.as_deref(),
            )
        })
        .await
        .map_err(|e| ToolError::Msg(format!("Veles task failed: {e}")))?
        .map_err(|e| ToolError::Msg(format!("Veles search failed: {e}")))?;

        let output = truncate_live_tool_output(Self::NAME, &output);
        Ok(match coaching {
            Some(coaching) => format!("{coaching}\n\n{output}"),
            None => output,
        })
    }
}

pub(crate) fn search_repo(
    path: &Path,
    cache_base: &Path,
    query: &str,
    top_k: usize,
    mode: SearchMode,
    languages: Option<&[String]>,
) -> anyhow::Result<String> {
    search_scoped(path, None, cache_base, query, top_k, mode, languages)
}

pub(crate) fn search_scoped(
    cwd: &Path,
    requested_path: Option<&str>,
    cache_base: &Path,
    query: &str,
    top_k: usize,
    mode: SearchMode,
    languages: Option<&[String]>,
) -> anyhow::Result<String> {
    let (workspace, identity, include) = resolve_search_scope(cwd, requested_path)?;
    let cache_host = cache_host_for(&identity, cache_base);
    std::fs::create_dir_all(&cache_host)?;

    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(cache_host.join("index.lock"))?;
    lock.lock()?;

    let index = if veles_core::persist::index_exists(&cache_host) {
        let model = veles_core::model::load_model(None)?;
        match VelesIndex::load(&cache_host, model) {
            Ok(mut index) => {
                let report = index.update_from_path(&workspace)?;
                if !report.is_noop() {
                    index.save(&cache_host)?;
                }
                index
            }
            Err(_) => {
                let index = VelesIndex::from_path(&workspace, None, None, false)?;
                index.save(&cache_host)?;
                index
            }
        }
    } else {
        let index = VelesIndex::from_path(&workspace, None, None, false)?;
        index.save(&cache_host)?;
        index
    };
    drop(lock);

    let mut filter_paths = veles_core::filter::resolve_path_filter(&index, &include, &[])?;
    let effective_languages = if let (Some(paths), Some(languages)) = (&mut filter_paths, languages)
    {
        paths.retain(|path| {
            index.chunks().iter().any(|chunk| {
                chunk.file_path == *path
                    && chunk
                        .language
                        .as_ref()
                        .is_some_and(|language| languages.contains(language))
            })
        });
        if paths.is_empty() {
            anyhow::bail!("No indexed files matched both the path and language filters");
        }
        None
    } else {
        languages
    };
    let results = index.search(
        query,
        top_k,
        mode,
        None,
        effective_languages,
        filter_paths.as_deref(),
    );
    Ok(format_results(&results))
}

fn resolve_search_scope(
    cwd: &Path,
    requested_path: Option<&str>,
) -> anyhow::Result<(PathBuf, PathBuf, Vec<String>)> {
    let (current_workspace, current_identity) = repository_paths(cwd)?;
    let Some(requested) = requested_path else {
        return Ok((current_workspace, current_identity, Vec::new()));
    };

    let expanded = PathBuf::from(crate::fs::expand_tilde(requested));
    let is_absolute = expanded.is_absolute();
    let target = if is_absolute {
        expanded
    } else {
        cwd.join(expanded)
    }
    .canonicalize()?;
    if !is_absolute && !target.starts_with(&current_workspace) {
        anyhow::bail!("relative search path escapes the current repository");
    }

    let (workspace, identity) = if target.starts_with(&current_workspace) {
        (current_workspace, current_identity)
    } else {
        let probe = if target.is_dir() {
            target.as_path()
        } else {
            target
                .parent()
                .ok_or_else(|| anyhow::anyhow!("search path has no parent"))?
        };
        repository_paths(probe)?
    };
    let relative = target.strip_prefix(&workspace).map_err(|_| {
        anyhow::anyhow!(
            "search path {} is outside indexed root {}",
            target.display(),
            workspace.display()
        )
    })?;
    if relative.as_os_str().is_empty() {
        return Ok((workspace, identity, Vec::new()));
    }

    let relative = relative.to_string_lossy().replace('\\', "/");
    let include = if target.is_dir() {
        vec![format!("{relative}/**")]
    } else {
        vec![relative]
    };
    Ok((workspace, identity, include))
}

pub(crate) fn repository_paths(path: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let workspace = git_path(path, "--show-toplevel")?.unwrap_or(path.canonicalize()?);
    let identity = git_path(path, "--git-common-dir")?.unwrap_or_else(|| workspace.clone());
    Ok((workspace, identity))
}

fn git_path(cwd: &Path, argument: &str) -> anyhow::Result<Option<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", argument])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    Ok(Some(path.canonicalize()?))
}

fn cache_base_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|base| base.join("zerostack"))
        .unwrap_or_else(crate::session::storage::data_dir)
        .join("veles")
}

pub(crate) fn cache_host_for(identity: &Path, base: &Path) -> PathBuf {
    let digest = Sha256::digest(identity.to_string_lossy().as_bytes());
    base.join(format!("{digest:x}"))
}

pub(crate) fn format_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "No matches found.".to_string();
    }
    results
        .iter()
        .map(|result| {
            format!(
                "{} [{:.4}, {}]\n```{}\n{}\n```",
                result.chunk.location(),
                result.score,
                result.source,
                result.chunk.language.as_deref().unwrap_or("text"),
                result.chunk.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}
