use serde::{Deserialize, Serialize};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Serialize, Deserialize)]
pub(crate) struct Job {
    pub(crate) job: String,
    repo: PathBuf,
    branch: String,
    pub(crate) path: PathBuf,
    base: String,
    description: Option<String>,
    status: String,
    pub(crate) log: PathBuf,
    error: Option<String>,
    #[serde(default)]
    created_at_ms: Option<u64>,
    #[serde(default)]
    finished_at_ms: Option<u64>,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    phases: Vec<PhaseTiming>,
}

#[derive(Serialize, Deserialize)]
struct PhaseTiming {
    phase: String,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    duration_ms: u64,
}

impl Job {
    fn transition(&mut self, status: &str, now: u64) {
        if let Some(phase) = self.phases.last_mut() {
            phase.finished_at_ms = Some(now);
        }
        self.status = status.into();
        if self.terminal() {
            self.finished_at_ms = Some(now);
        } else {
            self.phases.push(PhaseTiming {
                phase: status.into(),
                started_at_ms: now,
                finished_at_ms: None,
                duration_ms: 0,
            });
        }
        self.refresh_timing(now);
    }

    fn terminal(&self) -> bool {
        matches!(self.status.as_str(), "ready" | "failed" | "interrupted")
    }

    fn refresh_timing(&mut self, now: u64) {
        self.elapsed_ms = self
            .created_at_ms
            .map(|start| self.finished_at_ms.unwrap_or(now).saturating_sub(start));
        for phase in &mut self.phases {
            phase.duration_ms = phase
                .finished_at_ms
                .unwrap_or(now)
                .saturating_sub(phase.started_at_ms);
        }
    }
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
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
    let mut job: Job = serde_json::from_slice(&std::fs::read(dir.join("status.json"))?)?;
    job.refresh_timing(now_ms());
    Ok(job)
}

pub fn start(
    repo: &Path,
    branch: &str,
    path: &Path,
    base: &str,
    description: Option<&str>,
) -> anyhow::Result<Job> {
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
        created_at_ms: Some(now_ms()),
        finished_at_ms: None,
        elapsed_ms: None,
        phases: Vec::new(),
    };
    job.transition("queued", job.created_at_ms.unwrap());
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
            job.transition("failed", now_ms());
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
            job.transition("failed", now_ms());
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
    load(&dir)
}

fn publish_liveness(dir: &Path) -> anyhow::Result<std::fs::File> {
    let temporary = dir.join(format!("alive.{}.tmp", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.lock()?;
    let published = std::fs::hard_link(&temporary, dir.join("alive"));
    std::fs::remove_file(temporary)?;
    published?;
    Ok(file)
}

fn worker_alive(dir: &Path) -> anyhow::Result<bool> {
    let path = dir.join("alive");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_socket() {
        return Ok(UnixStream::connect(path).is_ok());
    }
    anyhow::ensure!(
        metadata.is_file(),
        "invalid worker liveness file: {}",
        path.display()
    );
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

pub fn run(id: &str) -> anyhow::Result<()> {
    let dir = directory(id)?;
    let mut job = load(&dir)?;
    let _liveness = publish_liveness(&dir)?;
    let result = super::git_worktree::create_workspace_observed(
        &job.repo.clone(),
        &job.branch.clone(),
        &job.path.clone(),
        Some(&job.base.clone()),
        job.description.clone().as_deref(),
        &mut |phase| {
            job.transition(phase, now_ms());
            eprintln!("workspace phase: {phase}");
            save(&dir, &job).map_err(|e| e.to_string())
        },
        true,
    );
    match result {
        Ok((path, _)) => {
            job.path = path;
            job.transition("ready", now_ms());
        }
        Err(error) => {
            eprintln!("{error}");
            job.transition("failed", now_ms());
            job.error = Some(error);
        }
    }
    save(&dir, &job)?;
    std::fs::remove_file(dir.join("alive"))?;
    Ok(())
}

fn read_status(dir: &Path) -> anyhow::Result<Job> {
    let mut job = load(dir)?;
    if !job.terminal() && !worker_alive(dir)? {
        job = load(dir)?;
        if !job.terminal() {
            job.transition("interrupted", now_ms());
            job.error =
                Some("worker stopped before recording completion; workspace may be partial".into());
        }
    }
    Ok(job)
}

pub fn status(id: &str) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string(&read_status(&directory(id)?)?)?);
    Ok(())
}

pub async fn wait(id: &str, timeout_secs: u64) -> anyhow::Result<()> {
    let job = wait_ready(id, timeout_secs).await?;
    let mut result = serde_json::to_value(job)?;
    result["timed_out"] = false.into();
    println!("{result}");
    Ok(())
}

pub async fn wait_ready(id: &str, timeout_secs: u64) -> anyhow::Result<Job> {
    let (job, timed_out) = wait_job(
        &directory(id)?,
        std::time::Duration::from_secs(timeout_secs),
    )
    .await?;
    if let Err(error) = ensure_ready(&job, timed_out) {
        let mut result = serde_json::to_value(&job)?;
        result["timed_out"] = timed_out.into();
        println!("{result}");
        return Err(error);
    }
    Ok(job)
}

fn ensure_ready(job: &Job, timed_out: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !timed_out,
        "workspace wait timed out; job {} continues; log {}",
        job.job,
        job.log.display()
    );
    anyhow::ensure!(
        job.status == "ready",
        "workspace job {} {}: {}; log {}",
        job.job,
        job.status,
        job.error.as_deref().unwrap_or("no error recorded"),
        job.log.display()
    );
    Ok(())
}

async fn wait_job(dir: &Path, timeout: std::time::Duration) -> anyhow::Result<(Job, bool)> {
    let started = std::time::Instant::now();
    let mut previous = None;
    let mut last_report = started;
    loop {
        let job = read_status(dir)?;
        let elapsed = started.elapsed();
        let timed_out = !job.terminal() && elapsed >= timeout;
        if previous.as_deref() != Some(job.status.as_str())
            || last_report.elapsed() >= std::time::Duration::from_secs(10)
            || timed_out
        {
            eprintln!(
                "workspace {}: {} | elapsed {} | phase {} | log {}{}",
                job.job,
                job.status,
                job.elapsed_ms
                    .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                    .unwrap_or_else(|| "unknown".into()),
                job.phases
                    .last()
                    .map(|phase| format!("{:.1}s", phase.duration_ms as f64 / 1000.0))
                    .unwrap_or_else(|| "unknown".into()),
                job.log.display(),
                if timed_out {
                    " | wait timed out; setup continues"
                } else {
                    ""
                }
            );
            previous = Some(job.status.clone());
            last_report = std::time::Instant::now();
        }
        if job.terminal() || timed_out {
            return Ok((job, timed_out));
        }
        tokio::time::sleep(
            std::time::Duration::from_millis(250).min(timeout.saturating_sub(elapsed)),
        )
        .await;
    }
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
    use std::os::unix::net::UnixListener;

    fn fixture() -> (PathBuf, Job) {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let job: Job = serde_json::from_value(serde_json::json!({
            "job": "test", "repo": dir, "path": dir, "branch": "task",
            "base": "HEAD", "description": null, "status": "queued",
            "log": dir.join("output.log"), "error": null
        }))
        .unwrap();
        (dir, job)
    }

    #[test]
    fn phase_timings_track_live_and_completed_durations() {
        let (dir, mut job) = fixture();
        job.created_at_ms = Some(1000);
        job.transition("queued", 1000);
        job.transition("preparing", 1100);
        job.refresh_timing(1400);
        assert_eq!(job.elapsed_ms, Some(400));
        assert_eq!(job.phases[0].duration_ms, 100);
        assert_eq!(job.phases[1].duration_ms, 300);
        job.transition("ready", 1600);
        job.refresh_timing(9000);
        assert_eq!(job.elapsed_ms, Some(600));
        assert_eq!(job.phases[1].duration_ms, 500);
        assert_eq!(job.finished_at_ms, Some(1600));
        job.refresh_timing(500);
        assert_eq!(job.elapsed_ms, Some(600));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_jobs_load_without_fabricated_timings() {
        let (dir, job) = fixture();
        assert!(job.created_at_ms.is_none());
        save(&dir, &job).unwrap();
        let job = load(&dir).unwrap();
        assert!(job.elapsed_ms.is_none());
        assert!(job.phases.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wait_observes_readiness_after_phase_changes() {
        let (dir, mut job) = fixture();
        let listener = UnixListener::bind(dir.join("alive")).unwrap();
        job.created_at_ms = Some(now_ms());
        job.transition("hydrating", now_ms());
        save(&dir, &job).unwrap();
        let worker_dir = dir.clone();
        let worker = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            job.transition("ready", now_ms());
            save(&worker_dir, &job).unwrap();
        });
        let (job, timed_out) = wait_job(&dir, std::time::Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(job.status, "ready");
        assert!(!timed_out);
        assert!(ensure_ready(&job, timed_out).is_ok());
        assert!(job.phases[0].finished_at_ms.is_some());
        worker.await.unwrap();
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wait_reports_failure_and_interruption_without_success() {
        let (dir, mut job) = fixture();
        job.transition("failed", now_ms());
        job.error = Some("hydrate hook failed".into());
        save(&dir, &job).unwrap();
        let (failed, timed_out) = wait_job(&dir, std::time::Duration::ZERO).await.unwrap();
        assert!(!timed_out);
        assert!(
            ensure_ready(&failed, timed_out)
                .unwrap_err()
                .to_string()
                .contains("hydrate hook failed")
        );
        job.status = "hydrating".into();
        save(&dir, &job).unwrap();
        let (interrupted, timed_out) = wait_job(&dir, std::time::Duration::ZERO).await.unwrap();
        assert_eq!(interrupted.status, "interrupted");
        assert!(!timed_out);
        assert!(ensure_ready(&interrupted, timed_out).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wait_timeout_leaves_worker_and_job_untouched() {
        let (dir, mut job) = fixture();
        let listener = UnixListener::bind(dir.join("alive")).unwrap();
        job.transition("hydrating", now_ms());
        save(&dir, &job).unwrap();
        let before = std::fs::read(dir.join("status.json")).unwrap();
        let (pending, timed_out) = wait_job(&dir, std::time::Duration::from_millis(20))
            .await
            .unwrap();
        assert!(timed_out);
        assert_eq!(pending.status, "hydrating");
        assert!(
            ensure_ready(&pending, timed_out)
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert_eq!(std::fs::read(dir.join("status.json")).unwrap(), before);
        assert!(UnixStream::connect(dir.join("alive")).is_ok());
        drop(listener);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn wait_rejects_missing_and_corrupt_records() {
        let (dir, _) = fixture();
        assert!(wait_job(&dir, std::time::Duration::ZERO).await.is_err());
        std::fs::write(dir.join("status.json"), b"not json").unwrap();
        assert!(wait_job(&dir, std::time::Duration::ZERO).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backwards_clock_does_not_underflow_durations() {
        let (dir, mut job) = fixture();
        job.created_at_ms = Some(1000);
        job.transition("hydrating", 1000);
        job.refresh_timing(500);
        assert_eq!(job.elapsed_ms, Some(0));
        assert_eq!(job.phases[0].duration_ms, 0);
        job.transition("failed", 700);
        assert_eq!(job.elapsed_ms, Some(0));
        assert_eq!(job.phases[0].duration_ms, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn liveness_lock_releases_on_worker_exit_and_rejects_duplicate_workers() {
        let (dir, _) = fixture();
        assert!(!worker_alive(&dir).unwrap());
        let worker = publish_liveness(&dir).unwrap();
        assert!(worker_alive(&dir).unwrap());
        assert!(publish_liveness(&dir).is_err());
        drop(worker);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while worker_alive(&dir).unwrap() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!worker_alive(&dir).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn liveness_rejects_invalid_registrations() {
        let (dir, _) = fixture();
        std::fs::create_dir(dir.join("alive")).unwrap();
        assert!(worker_alive(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn worker_supports_long_data_directory_paths() {
        let (root, mut job) = fixture();
        job.job = uuid::Uuid::new_v4().to_string();
        job.repo = root.join("repo");
        job.path = root.join("workspace");
        std::fs::create_dir(&job.repo).unwrap();
        for args in [
            vec!["init"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        ] {
            let output = Command::new("git")
                .arg("-C")
                .arg(&job.repo)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let data = root.join("x".repeat(150));
        let previous = crate::session::storage::set_test_data_dir(Some(data));
        let dir = directory(&job.job).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &job).unwrap();
        let result = run(&job.job);
        crate::session::storage::set_test_data_dir(previous);
        result.unwrap();
        assert_eq!(load(&dir).unwrap().status, "ready");
        std::fs::remove_dir_all(root).unwrap();
    }

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
            created_at_ms: None,
            finished_at_ms: None,
            elapsed_ms: None,
            phases: Vec::new(),
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
