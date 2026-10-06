use serde::{Deserialize, Serialize};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Serialize, Deserialize)]
struct Job {
    job: String,
    repo: PathBuf,
    branch: String,
    path: PathBuf,
    base: String,
    description: Option<String>,
    status: String,
    log: PathBuf,
    error: Option<String>,
}

fn directory(id: &str) -> anyhow::Result<PathBuf> {
    let id = uuid::Uuid::parse_str(id)?;
    Ok(crate::session::storage::data_dir()
        .join("workspace-jobs")
        .join(id.to_string()))
}

fn save(dir: &Path, job: &Job) -> anyhow::Result<()> {
    let temporary = dir.join(format!("status.{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec(job)?)?;
    std::fs::rename(temporary, dir.join("status.json"))?;
    Ok(())
}

fn load(dir: &Path) -> anyhow::Result<Job> {
    Ok(serde_json::from_slice(&std::fs::read(
        dir.join("status.json"),
    )?)?)
}

pub fn start(
    repo: &Path,
    branch: &str,
    path: &Path,
    base: &str,
    description: Option<&str>,
) -> anyhow::Result<()> {
    let repo = repo.canonicalize()?;
    anyhow::ensure!(repo.is_dir(), "repository must be a directory");
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo.join(path)
    };
    anyhow::ensure!(
        !path.exists(),
        "workspace path already exists: {}",
        path.display()
    );
    let id = uuid::Uuid::new_v4().to_string();
    let dir = directory(&id)?;
    std::fs::create_dir_all(&dir)?;
    let log_path = dir.join("output.log");
    let log = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&log_path)?;
    let mut job = Job {
        job: id.clone(),
        repo,
        branch: branch.into(),
        path,
        base: base.into(),
        description: description.map(Into::into),
        status: "queued".into(),
        log: log_path,
        error: None,
    };
    save(&dir, &job)?;
    let spawned = Command::new(std::env::current_exe()?)
        .args(["workspace", "run-job", "--job", &id])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            job.status = "failed".into();
            job.error = Some(error.to_string());
            save(&dir, &job)?;
            return Err(error.into());
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let current = load(&dir)?;
        if matches!(current.status.as_str(), "ready" | "failed") || dir.join("alive").exists() {
            break;
        }
        if let Some(exit) = child.try_wait()? {
            job.status = "failed".into();
            job.error = Some(format!("worker exited before startup: {exit}"));
            save(&dir, &job)?;
            break;
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "worker startup not confirmed; inspect job {id}; log {}",
                job.log.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    println!("{}", serde_json::to_string(&load(&dir)?)?);
    Ok(())
}

pub fn run(id: &str) -> anyhow::Result<()> {
    let dir = directory(id)?;
    let mut job = load(&dir)?;
    let socket = dir.join("alive");
    let listener = UnixListener::bind(&socket)?;
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            if connection.is_err() {
                break;
            }
        }
    });
    let result = super::git_worktree::create_workspace_observed(
        &job.repo.clone(),
        &job.branch.clone(),
        &job.path.clone(),
        Some(&job.base.clone()),
        job.description.clone().as_deref(),
        &mut |phase| {
            job.status = phase.into();
            eprintln!("workspace phase: {phase}");
            save(&dir, &job).map_err(|e| e.to_string())
        },
        true,
    );
    match result {
        Ok((path, _)) => {
            job.path = path;
            job.status = "ready".into();
        }
        Err(error) => {
            eprintln!("{error}");
            job.status = "failed".into();
            job.error = Some(error);
        }
    }
    save(&dir, &job)?;
    std::fs::remove_file(socket)?;
    Ok(())
}

pub fn status(id: &str) -> anyhow::Result<()> {
    let dir = directory(id)?;
    let mut job = load(&dir)?;
    if !matches!(job.status.as_str(), "ready" | "failed")
        && UnixStream::connect(dir.join("alive")).is_err()
    {
        job = load(&dir)?;
        if !matches!(job.status.as_str(), "ready" | "failed") {
            job.status = "interrupted".into();
            job.error =
                Some("worker stopped before recording completion; workspace may be partial".into());
        }
    }
    println!("{}", serde_json::to_string(&job)?);
    Ok(())
}

pub fn logs(id: &str) -> anyhow::Result<()> {
    let dir = directory(id)?;
    let mut file = std::fs::File::open(dir.join("output.log"))?;
    std::io::copy(&mut file, &mut std::io::stdout())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_job_path_traversal() {
        assert!(directory("../other").is_err());
    }

    #[test]
    fn status_round_trip_and_atomic_replacement() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let mut job = Job {
            job: "test".into(),
            repo: dir.clone(),
            path: dir.clone(),
            branch: "task".into(),
            base: "HEAD".into(),
            description: None,
            status: "hydrating".into(),
            log: dir.join("output.log"),
            error: None,
        };
        save(&dir, &job).unwrap();
        assert_eq!(load(&dir).unwrap().status, "hydrating");
        job.status = "failed".into();
        job.error = Some("hook failed".into());
        save(&dir, &job).unwrap();
        assert_eq!(load(&dir).unwrap().error.as_deref(), Some("hook failed"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
