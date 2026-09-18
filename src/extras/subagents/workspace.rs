use std::io::Write;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) fn create(source: &Path, session_id: &str) -> Result<PathBuf, String> {
    let source = source
        .canonicalize()
        .map_err(|e| format!("failed to resolve workspace source: {e}"))?;
    let target = crate::session::storage::data_dir()
        .join("subagents")
        .join(session_id)
        .join("workspace");
    if target.exists() {
        return Err(format!(
            "subagent workspace already exists: {}",
            target.display()
        ));
    }
    std::fs::create_dir_all(
        target
            .parent()
            .ok_or_else(|| "subagent workspace has no parent".to_string())?,
    )
    .map_err(|e| format!("failed to create subagent workspace directory: {e}"))?;

    match git_root(&source) {
        Some(root) => create_git_snapshot(&root, &source, &target),
        None => copy_tree(&source, &target).map(|_| target),
    }
}

fn git_root(source: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    output.status.success().then(|| {
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
            .canonicalize()
            .ok()
    })?
}

fn create_git_snapshot(root: &Path, source: &Path, target: &Path) -> Result<PathBuf, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["worktree", "add", "--detach"])
        .arg(target)
        .arg("HEAD")
        .output()
        .map_err(|e| format!("failed to create subagent worktree: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "failed to create subagent worktree: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let result = (|| {
        let diff = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--binary", "--full-index", "HEAD"])
            .output()
            .map_err(|e| format!("failed to capture in-progress edits: {e}"))?;
        if !diff.status.success() {
            return Err(format!(
                "failed to capture in-progress edits: {}",
                String::from_utf8_lossy(&diff.stderr).trim()
            ));
        }
        if !diff.stdout.is_empty() {
            let mut child = Command::new("git")
                .arg("-C")
                .arg(target)
                .args(["apply", "--whitespace=nowarn", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("failed to apply in-progress edits: {e}"))?;
            child
                .stdin
                .take()
                .ok_or_else(|| "failed to open git apply stdin".to_string())?
                .write_all(&diff.stdout)
                .map_err(|e| format!("failed to send in-progress edits to git apply: {e}"))?;
            let applied = child
                .wait_with_output()
                .map_err(|e| format!("failed to apply in-progress edits: {e}"))?;
            if !applied.status.success() {
                return Err(format!(
                    "failed to apply in-progress edits: {}",
                    String::from_utf8_lossy(&applied.stderr).trim()
                ));
            }
        }
        copy_untracked(root, target)?;
        let relative = source.strip_prefix(root).unwrap_or(Path::new(""));
        Ok(target.join(relative))
    })();

    if result.is_err() {
        let _ = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["worktree", "remove", "--force"])
            .arg(target)
            .output();
    }
    result
}

fn copy_untracked(root: &Path, target: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()
        .map_err(|e| format!("failed to list untracked files: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "failed to list untracked files: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    for bytes in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = path_from_git_bytes(bytes);
        let destination = target.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        copy_path(&root.join(&relative), &destination)?;
    }
    Ok(())
}

fn path_from_git_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).as_ref())
    }
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target)
        .map_err(|e| format!("failed to create {}: {e}", target.display()))?;
    let source_contents = source.join(".");
    copy_path(&source_contents, target)
}

fn copy_path(source: &Path, target: &Path) -> Result<(), String> {
    let output = Command::new("cp")
        .args(["-a", "--reflink=auto", "--"])
        .arg(source)
        .arg(target)
        .output()
        .map_err(|e| format!("failed to copy {}: {e}", source.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "failed to copy {}: {}",
            source.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
