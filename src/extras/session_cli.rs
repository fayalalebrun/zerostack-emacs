use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Context;

pub async fn start(
    path: &Path,
    provider: Option<&str>,
    model: Option<&str>,
    prompt: Option<&str>,
) -> anyhow::Result<()> {
    let path = path.canonicalize().context("workspace does not exist")?;
    anyhow::ensure!(path.is_dir(), "workspace must be a directory");
    anyhow::ensure!(
        !prompt.is_some_and(|p| p.trim().is_empty()),
        "prompt must not be empty"
    );
    let id = uuid::Uuid::new_v4().to_string();
    let logs = crate::session::storage::data_dir().join("session-logs");
    std::fs::create_dir_all(&logs)?;
    let log_path = logs.join(format!("{id}.log"));
    let log = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--emacs", "--subagent-session-id", &id])
        .current_dir(&path)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    if let Some(provider) = provider {
        command.args(["--provider", provider]);
    }
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    let mut child = command.spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "session startup failed ({status}); see {}",
                log_path.display()
            );
        }
        if crate::extras::emacs::session_socket_path(&id).exists()
            && crate::extras::emacs::cli_request(&id, None).await.is_ok()
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            anyhow::bail!("session startup timed out; see {}", log_path.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let metadata = serde_json::json!({"session": id, "pid": child.id(), "path": path,
        "socket": crate::extras::emacs::session_socket_path(&id), "log": log_path});
    if let Some(prompt) = prompt {
        if let Err(error) = crate::extras::emacs::cli_request(&id, Some(prompt)).await {
            anyhow::bail!("session started: {metadata}; initial prompt failed: {error}");
        }
    }
    println!("{metadata}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_workspace() {
        let missing = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        assert!(start(&missing, None, None, None).await.is_err());
    }

    #[tokio::test]
    async fn rejects_file_workspace_and_empty_prompt() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::write(&path, "fixture").unwrap();
        assert!(start(&path, None, None, None).await.is_err());
        std::fs::remove_file(path).unwrap();
        assert!(
            start(&std::env::temp_dir(), None, None, Some(" "))
                .await
                .is_err()
        );
    }
}
