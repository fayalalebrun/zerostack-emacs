use clap::Parser;
use std::ffi::OsString;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Context;

pub async fn start(
    path: &Path,
    session: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    prompt: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !prompt.is_some_and(|p| p.trim().is_empty()),
        "prompt must not be empty"
    );
    let mut args: Vec<OsString> = Vec::new();
    for (flag, value) in [
        ("--session", session),
        ("--provider", provider),
        ("--model", model),
    ] {
        if let Some(value) = value {
            args.extend([flag.into(), value.into()]);
        }
    }
    let metadata = launch(path, args).await?;
    if let Some(prompt) = prompt {
        let id = metadata["session"]
            .as_str()
            .context("missing session identity")?;
        if let Err(error) = crate::extras::emacs::cli_request(id, Some(prompt)).await {
            anyhow::bail!("session started: {metadata}; initial prompt failed: {error}");
        }
    }
    println!("{metadata}");
    Ok(())
}

fn launch_identity(args: &mut Vec<OsString>) -> anyhow::Result<String> {
    let cli = crate::cli::Cli::try_parse_from(
        std::iter::once(OsString::from("zerostack")).chain(args.iter().cloned()),
    )?;
    anyhow::ensure!(
        !cli.emacs_launch && cli.command.is_none() && !cli.print && !cli.print_config,
        "invalid daemon startup mode"
    );
    if let Some(prefix) = cli.session.as_deref() {
        let sessions = crate::session::storage::find_sessions_by_prefix(prefix)?;
        anyhow::ensure!(
            sessions.len() == 1,
            "session prefix must match exactly one saved session: {prefix}"
        );
        return Ok(sessions[0].id.to_string());
    }
    if cli.continue_session
        && let Some(session) = crate::session::storage::find_recent_sessions(1)?.first()
    {
        let index = args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(args.len());
        args.splice(
            index..index,
            ["--session".into(), session.id.to_string().into()],
        );
        return Ok(session.id.to_string());
    }
    let id = cli
        .subagent_session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    uuid::Uuid::parse_str(&id).context("invalid session identity")?;
    if !args.iter().any(|arg| {
        arg == "--subagent-session-id"
            || arg.to_string_lossy().starts_with("--subagent-session-id=")
    }) {
        let index = args
            .iter()
            .position(|arg| arg == "--")
            .unwrap_or(args.len());
        args.splice(
            index..index,
            ["--subagent-session-id".into(), id.clone().into()],
        );
    }
    Ok(id)
}

pub async fn launch(path: &Path, mut args: Vec<OsString>) -> anyhow::Result<serde_json::Value> {
    let path = path.canonicalize().context("workspace does not exist")?;
    anyhow::ensure!(path.is_dir(), "workspace must be a directory");
    let id = launch_identity(&mut args)?;
    uuid::Uuid::parse_str(&id).context("invalid saved session identity")?;
    let socket = crate::extras::emacs::session_socket_path(&id);
    if socket.exists() && crate::extras::emacs::cli_request(&id, None).await.is_ok() {
        anyhow::bail!("session {id} is already running");
    }
    let logs = crate::session::storage::data_dir().join("session-logs");
    std::fs::create_dir_all(&logs)?;
    let unit = format!("zerostack-{id}-{}", uuid::Uuid::new_v4());
    let log_path = logs.join(format!("{unit}.log"));
    let log = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log_path)?;
    drop(log);
    eprintln!("log {}", log_path.display());
    let mut command = service_command(&path, &log_path, &unit, &std::env::current_exe()?, &args);
    let output = command.output().context(
        "launch systemd user service (systemd-run and an active user manager are required)",
    )?;
    anyhow::ensure!(
        output.status.success(),
        "service startup failed: {}; see {}",
        String::from_utf8_lossy(&output.stderr).trim(),
        log_path.display()
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut last_probe = tokio::time::Instant::now() - Duration::from_secs(1);
    loop {
        if socket.exists() && crate::extras::emacs::cli_request(&id, None).await.is_ok() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let stopped = Command::new("systemctl")
                .args(["--user", "stop", &unit])
                .status()?;
            anyhow::ensure!(
                stopped.success(),
                "startup timed out and could not stop {unit}; see {}",
                log_path.display()
            );
            anyhow::bail!("session startup timed out; see {}", log_path.display());
        }
        if last_probe.elapsed() >= Duration::from_millis(500) {
            let alive = Command::new("systemctl")
                .args(["--user", "is-active", "--quiet", &unit])
                .status()?;
            if !alive.success() {
                let output = std::fs::read_to_string(&log_path).unwrap_or_default();
                anyhow::bail!(
                    "session startup failed: {}; see {}",
                    output.trim(),
                    log_path.display()
                );
            }
            last_probe = tokio::time::Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let pid: u32 = std::fs::read_to_string(
        socket
            .parent()
            .context("socket has no directory")?
            .join("pid"),
    )?
    .trim()
    .parse()?;
    Ok(
        serde_json::json!({"session": id, "pid": pid, "path": path, "socket": socket, "log": log_path, "unit": unit}),
    )
}

fn service_command(
    path: &Path,
    log: &Path,
    unit: &str,
    executable: &Path,
    args: &[OsString],
) -> Command {
    let mut command = Command::new("systemd-run");
    command
        .args([
            "--user",
            "--collect",
            "--quiet",
            "--service-type=exec",
            "--expand-environment=no",
        ])
        .arg(format!("--unit={unit}"))
        .arg(format!("--working-directory={}", path.display()))
        .arg("--property=StandardInput=null")
        .arg(format!(
            "--property=StandardOutput=append:{}",
            log.display()
        ))
        .arg(format!("--property=StandardError=append:{}", log.display()))
        .stdin(Stdio::null());
    for (name, _) in std::env::vars_os() {
        if let Some(name) = name.to_str()
            && !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            command.arg(format!("--setenv={name}"));
        }
    }
    command.arg("--").arg(executable).arg("--emacs").args(args);
    command
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_service_command_preserves_arguments_and_inherits_environment_by_name() {
        let arguments = vec![
            OsString::from("--model"),
            OsString::from("literal $value with spaces"),
        ];
        let command = service_command(
            Path::new("/tmp/work space"),
            Path::new("/tmp/private.log"),
            "test-unit",
            Path::new("/bin/zerostack"),
            &arguments,
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(command.get_program(), "systemd-run");
        assert!(args.contains(&"--expand-environment=no".into()));
        assert!(args.contains(&"--working-directory=/tmp/work space".into()));
        assert!(args.contains(&"--property=StandardInput=null".into()));
        assert!(args.contains(&"--property=StandardError=append:/tmp/private.log".into()));
        assert!(
            args.iter()
                .filter_map(|arg| arg.strip_prefix("--setenv="))
                .all(|name| !name.contains('='))
        );
        assert!(args.ends_with(&[
            "--".into(),
            "/bin/zerostack".into(),
            "--emacs".into(),
            "--model".into(),
            "literal $value with spaces".into()
        ]));
    }

    #[test]
    fn generates_pinned_identity_and_rejects_recursive_or_invalid_launches() {
        let mut args = vec!["--no-tools".into()];
        let id = launch_identity(&mut args).unwrap();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert_eq!(launch_identity(&mut args).unwrap(), id);
        for args in [
            ["--emacs-launch"],
            ["--print"],
            ["--subagent-session-id=invalid"],
        ] {
            assert!(launch_identity(&mut args.into_iter().map(Into::into).collect()).is_err());
        }
    }

    #[test]
    fn pinned_identity_precedes_positional_argument_boundary() {
        let mut args = vec!["--".into(), "literal message".into()];
        let id = launch_identity(&mut args).unwrap();
        let cli = crate::cli::Cli::try_parse_from(
            std::iter::once(OsString::from("zerostack")).chain(args),
        )
        .unwrap();
        assert_eq!(cli.subagent_session_id.as_deref(), Some(id.as_str()));
        assert_eq!(cli.message, vec!["literal message"]);
    }

    #[test]
    fn resume_uses_the_saved_identity_without_creating_a_new_session() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let previous = crate::session::storage::set_test_data_dir(Some(dir.clone()));
        let session = crate::session::Session::new("test", "test", 1000);
        crate::session::storage::save_session(&session).unwrap();
        let mut args = vec!["--session".into(), session.id[..8].into()];
        let result = launch_identity(&mut args);
        let continued = launch_identity(&mut vec!["--continue".into()]);
        let missing = launch_identity(&mut vec!["--session=missing".into()]);
        crate::session::storage::set_test_data_dir(previous);
        assert_eq!(result.unwrap(), session.id.as_str());
        assert_eq!(continued.unwrap(), session.id.as_str());
        assert!(missing.is_err());
        std::fs::remove_file(dir.join("sessions").join(format!("{}.json", session.id))).unwrap();
        std::fs::remove_dir(dir.join("sessions")).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[tokio::test]
    async fn rejects_missing_workspace() {
        let missing = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        assert!(start(&missing, None, None, None, None).await.is_err());
    }

    #[tokio::test]
    async fn rejects_file_workspace_and_empty_prompt() {
        let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::write(&path, "fixture").unwrap();
        assert!(start(&path, None, None, None, None).await.is_err());
        std::fs::remove_file(path).unwrap();
        assert!(
            start(&std::env::temp_dir(), None, None, None, Some(" "))
                .await
                .is_err()
        );
    }
}
