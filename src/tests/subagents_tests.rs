//! Tests for the `subagents` feature.
//!
//! Run with: cargo test --features subagents
//!
//! These tests cover the pure-logic portions that don't require an actual
//! LLM: argument parsing, response truncation, result combining, and the
//! empty-prompts guard.

#[cfg(test)]
mod tests {

    // -----------------------------------------------------------------------
    // SpawnRequest deserialization
    // -----------------------------------------------------------------------

    #[test]
    fn task_args_deserialize_spawn_request() {
        let json = r#"{"task":"fix auth","access":"write","timeout":120,"model":"fast","reasoning":"high"}"#;
        let args: crate::extras::subagents::task_tool::SpawnRequest =
            serde_json::from_str(json).unwrap();
        assert_eq!(args.task, "fix auth");
        assert_eq!(
            args.access,
            crate::extras::subagents::task_tool::Access::Write
        );
        assert_eq!(args.timeout, 120);
        assert_eq!(args.model.as_deref(), Some("fast"));
        assert_eq!(args.reasoning.as_deref(), Some("high"));
    }

    #[test]
    fn task_args_allow_optional_model_and_reasoning() {
        let json = r#"{"task":"inspect auth","access":"read","timeout":30}"#;
        let args: crate::extras::subagents::task_tool::SpawnRequest =
            serde_json::from_str(json).unwrap();
        assert_eq!(
            args.access,
            crate::extras::subagents::task_tool::Access::Read
        );
        assert!(args.model.is_none());
        assert!(args.reasoning.is_none());
    }

    #[test]
    fn task_args_reject_invalid_access() {
        let json = r#"{"task":"inspect","access":"admin","timeout":30}"#;
        assert!(
            serde_json::from_str::<crate::extras::subagents::task_tool::SpawnRequest>(json)
                .is_err()
        );
    }

    #[test]
    fn configured_model_options_resolve_aliases_and_raw_ids() {
        use compact_str::CompactString;
        use std::collections::HashMap;

        let cfg = crate::config::Config {
            subagent_models: Some(vec!["fast".into(), "raw-model".into()]),
            quick_models: Some(HashMap::from([(
                "fast".to_string(),
                crate::config::QuickModelConfig {
                    provider: CompactString::new("openrouter"),
                    model: CompactString::new("vendor/fast"),
                    input_token_cost: 0.0,
                    output_token_cost: 0.0,
                    reserve_tokens: None,
                    temperature: None,
                    extra_body: None,
                    reasoning_effort: None,
                },
            )])),
            ..Default::default()
        };
        let options =
            crate::extras::subagents::resolve_model_options(&cfg, "anthropic", "fallback");
        assert_eq!(options[0].name, "fast");
        assert_eq!(options[0].provider, "openrouter");
        assert_eq!(options[0].model, "vendor/fast");
        assert_eq!(options[1].provider, "anthropic");
        assert_eq!(options[1].model, "raw-model");
    }

    #[test]
    fn blank_configured_model_options_fall_back_to_main_model() {
        let cfg = crate::config::Config {
            subagent_models: Some(vec!["  ".into()]),
            ..Default::default()
        };
        let options =
            crate::extras::subagents::resolve_model_options(&cfg, "anthropic", "fallback");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].name, "fallback");
        assert_eq!(options[0].provider, "anthropic");
        assert_eq!(options[0].model, "fallback");
    }

    #[test]
    fn spawn_model_must_be_in_permitted_options() {
        let options = vec![crate::extras::subagents::ModelOption {
            name: "allowed".to_string(),
            provider: "openrouter".to_string(),
            model: "vendor/model".to_string(),
        }];
        assert_eq!(
            crate::extras::subagents::task_tool::select_model_option(&options, None)
                .unwrap()
                .name,
            "allowed"
        );
        assert!(
            crate::extras::subagents::task_tool::select_model_option(&options, Some("forbidden"))
                .unwrap_err()
                .to_string()
                .contains("not permitted")
        );
    }

    #[test]
    fn parent_workspace_permissions_match_requested_access() {
        use crate::extras::subagents::task_tool::{Access, parent_workspace_tools};

        assert_eq!(parent_workspace_tools(Access::Read), &["read", "list_dir"]);
        assert_eq!(
            parent_workspace_tools(Access::Write),
            &["read", "write", "edit", "list_dir"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn aborting_parent_task_kills_child_process_group() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        let mut child = Command::new("bash");
        child
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        child.process_group(0);
        let mut child = child.spawn().unwrap();
        let pid = child.id();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = crate::extras::subagents::task_tool::ProcessGroupGuard::new(Some(pid));
            let _ = ready_tx.send(());
            std::future::pending::<()>().await;
        });

        ready_rx.await.unwrap();
        task.abort();
        let _ = task.await;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn missing_current_executable_falls_back_to_path() {
        use crate::extras::subagents::task_tool::resolve_subagent_executable;

        assert_eq!(
            resolve_subagent_executable(Some(std::path::PathBuf::from(
                "/definitely/missing/zerostack"
            ))),
            std::path::PathBuf::from("zerostack")
        );
    }

    #[test]
    fn subagent_reserves_last_twenty_percent_for_finalization() {
        use crate::extras::subagents::task_tool::finalize_after_ms;

        assert_eq!(finalize_after_ms(1), 800);
        assert_eq!(finalize_after_ms(10), 8_000);
        assert_eq!(finalize_after_ms(100), 80_000);
        assert_eq!(finalize_after_ms(u64::MAX), u64::MAX);
    }

    #[test]
    fn task_args_missing_required_field_is_error() {
        let json = r#"{"task":"inspect","access":"read"}"#;
        assert!(
            serde_json::from_str::<crate::extras::subagents::task_tool::SpawnRequest>(json)
                .is_err()
        );
    }

    // -----------------------------------------------------------------------
    // Response truncation
    // -----------------------------------------------------------------------

    const CAP: usize = 128 * 1024;
    const MARKER: &str = "\n…[subagent response truncated at 131072B]";

    #[test]
    fn truncate_response_preserves_short_string() {
        let s = "hello world";
        let result = crate::extras::truncate::truncate_cjk(s, CAP, MARKER);
        assert_eq!(result, s);
    }

    #[test]
    fn truncate_response_caps_long_string() {
        let s = "x".repeat(200 * 1024); // 200KB, well over the 128KB cap
        let result = crate::extras::truncate::truncate_cjk(&s, CAP, MARKER);
        assert!(result.len() <= 128 * 1024 + 64); // cap + marker overhead
        assert!(result.contains("[subagent response truncated"));
    }

    #[test]
    fn truncate_response_does_not_panic_on_cjk() {
        // Cutting mid-char would panic with a plain String::truncate
        let cjk = "記憶".repeat(50 * 1024); // plenty of multi-byte chars
        let result = crate::extras::truncate::truncate_cjk(&cjk, CAP, MARKER);
        // Must not panic; must contain the marker
        assert!(result.contains("[subagent response truncated"));
    }

    #[test]
    fn truncate_response_starts_with_prefix_of_original() {
        let s = "AAAABBBBCCCCDDDD";
        let result = crate::extras::truncate::truncate_cjk(s, CAP, MARKER);
        assert!(result.starts_with("AAAABB"));
    }

    #[test]
    fn write_workspace_contains_staged_unstaged_and_untracked_edits() {
        use std::process::Command;

        let root =
            std::env::temp_dir().join(format!("zs-subagent-snapshot-{}", uuid::Uuid::new_v4()));
        let data = root.join("data");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        std::fs::write(repo.join("staged.txt"), "base\n").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["add", "."])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["commit", "-m", "base"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(repo.join("tracked.txt"), "edited\n").unwrap();
        std::fs::write(repo.join("staged.txt"), "staged\n").unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["add", "staged.txt"])
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(repo.join("untracked.txt"), "new\n").unwrap();
        #[cfg(unix)]
        let non_utf8 = {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;

            let path = std::path::PathBuf::from(OsString::from_vec(b"untracked-\xff".to_vec()));
            std::fs::write(repo.join(&path), "non-utf8\n").unwrap();
            path
        };

        let previous = crate::session::storage::set_test_data_dir(Some(data));
        let workspace =
            crate::extras::subagents::workspace::create(&repo, "snapshot-test").unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("tracked.txt")).unwrap(),
            "edited\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("staged.txt")).unwrap(),
            "staged\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("untracked.txt")).unwrap(),
            "new\n"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_to_string(workspace.join(non_utf8)).unwrap(),
            "non-utf8\n"
        );
        crate::session::storage::set_test_data_dir(previous);
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["worktree", "remove", "--force"])
                .arg(&workspace)
                .status()
                .unwrap()
                .success()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
