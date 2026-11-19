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
        assert!(!args.worktree);
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
    fn task_worktree_is_optional_and_explicit() {
        use crate::extras::subagents::task_tool::{Access, SpawnRequest, prepare_workspace};

        let root =
            std::env::temp_dir().join(format!("zs-subagent-shared-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for access in ["read", "write"] {
            for worktree in [None, Some(false), Some(true)] {
                let mut request = serde_json::json!({
                    "task": "inspect", "access": access, "timeout": 30
                });
                if let Some(worktree) = worktree {
                    request["worktree"] = worktree.into();
                }
                let args: SpawnRequest = serde_json::from_value(request).unwrap();
                assert_eq!(args.worktree, worktree.unwrap_or(false));
                if args.access == Access::Write && args.worktree {
                    continue;
                }
                let workspace =
                    prepare_workspace(args.access, args.worktree, &root, "unused").unwrap();
                assert_eq!(workspace, root.canonicalize().unwrap());
                std::fs::write(workspace.join("shared.txt"), "shared edit").unwrap();
                assert_eq!(
                    std::fs::read_to_string(root.join("shared.txt")).unwrap(),
                    "shared edit"
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
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
    fn subagent_timeout_is_the_soft_finalization_deadline() {
        use crate::extras::subagents::task_tool::finalize_after_ms;

        assert_eq!(finalize_after_ms(1), 1_000);
        assert_eq!(finalize_after_ms(10), 10_000);
        assert_eq!(finalize_after_ms(100), 100_000);
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
        let workspace = crate::extras::subagents::task_tool::prepare_workspace(
            crate::extras::subagents::task_tool::Access::Write,
            true,
            &repo,
            "snapshot-test",
        )
        .unwrap();
        assert_ne!(workspace, repo.canonicalize().unwrap());
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

    struct ReuseFixture {
        root: std::path::PathBuf,
        previous_data: Option<std::path::PathBuf>,
        session: crate::session::Session,
        args: crate::extras::subagents::task_tool::SpawnRequest,
        models: Vec<crate::extras::subagents::ModelOption>,
    }

    impl ReuseFixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("zs-subagent-reuse-{}", uuid::Uuid::new_v4()));
            let workspace = root.join("saved-workspace");
            std::fs::create_dir_all(&workspace).unwrap();
            let previous_data = crate::session::storage::set_test_data_dir(Some(root.join("data")));
            let mut session = crate::session::Session::new("test-provider", "test-model", 1000);
            session.parent_session_id = Some("parent".into());
            session.subagent_access = Some("write".into());
            session.working_dir = workspace.to_str().unwrap().into();
            session.add_message(crate::session::MessageRole::User, "Remember the auth fix");
            session.add_message(
                crate::session::MessageRole::Assistant,
                "The fix is in auth.rs",
            );
            crate::session::storage::save_session(&session).unwrap();
            let args = serde_json::from_value(serde_json::json!({
                "task": "Now test that fix", "session_id": session.id,
                "access": "write", "timeout": 30
            }))
            .unwrap();
            let models = vec![
                crate::extras::subagents::ModelOption {
                    name: "new-default".into(),
                    provider: "test-provider".into(),
                    model: "other-model".into(),
                },
                crate::extras::subagents::ModelOption {
                    name: "saved-model".into(),
                    provider: "test-provider".into(),
                    model: "test-model".into(),
                },
            ];
            Self {
                root,
                previous_data,
                session,
                args,
                models,
            }
        }

        fn prepare(
            &self,
        ) -> Result<crate::extras::subagents::task_tool::PreparedTask, crate::agent::tools::ToolError>
        {
            crate::extras::subagents::task_tool::prepare_task(
                &self.args,
                "parent",
                &self.models,
                &self.root,
            )
        }

        fn save(&self) {
            crate::session::storage::save_session(&self.session).unwrap();
        }
    }

    impl Drop for ReuseFixture {
        fn drop(&mut self) {
            crate::session::storage::set_test_data_dir(self.previous_data.take());
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn reuse_keeps_session_workspace_model_and_history() {
        let mut fixture = ReuseFixture::new();
        // A worktree request must not snapshot the workspace again during reuse.
        fixture.args.worktree = true;
        let prepared = fixture.prepare().unwrap();
        assert_eq!(prepared.session_id, fixture.session.id.as_str());
        assert_eq!(
            prepared.workspace,
            std::path::Path::new(fixture.session.working_dir.as_str())
        );
        assert_eq!(prepared.model_name, "saved-model");
        let sessions =
            crate::session::storage::find_sessions_by_prefix(&prepared.session_id).unwrap();
        assert_eq!(sessions.len(), 1);
        let history = crate::agent::runner::convert_history(&sessions[0]);
        assert_eq!(history.len(), 2);
        assert!(format!("{history:?}").contains("The fix is in auth.rs"));
        assert_eq!(sessions[0].messages.len(), 2);
    }

    #[test]
    fn reuse_rejects_busy_sessions_and_releases_lock_after_completion() {
        let fixture = ReuseFixture::new();
        let prepared = fixture.prepare().unwrap();
        assert!(
            fixture
                .prepare()
                .err()
                .unwrap()
                .to_string()
                .contains("already running")
        );
        drop(prepared);
        assert!(fixture.prepare().is_ok());
    }

    #[test]
    fn reuse_rejects_foreign_sessions_and_access_changes() {
        let mut fixture = ReuseFixture::new();
        fixture.session.parent_session_id = Some("other-parent".into());
        fixture.save();
        assert!(
            fixture
                .prepare()
                .err()
                .unwrap()
                .to_string()
                .contains("does not belong")
        );
        fixture.session.parent_session_id = None;
        fixture.save();
        assert!(fixture.prepare().is_err());
        fixture.session.parent_session_id = Some("parent".into());
        fixture.session.subagent_access = Some("read".into());
        fixture.save();
        assert!(
            fixture
                .prepare()
                .err()
                .unwrap()
                .to_string()
                .contains("access level")
        );
    }

    #[test]
    fn reuse_enforces_saved_model_and_current_allowlist() {
        let mut fixture = ReuseFixture::new();
        fixture.args.model = Some("saved-model".into());
        assert!(fixture.prepare().is_ok());
        fixture.args.model = Some("new-default".into());
        assert!(
            fixture
                .prepare()
                .err()
                .unwrap()
                .to_string()
                .contains("model and provider")
        );
        fixture.args.model = Some("not-allowed".into());
        assert!(
            fixture
                .prepare()
                .err()
                .unwrap()
                .to_string()
                .contains("not permitted")
        );
        fixture.args.model = None;
        fixture.models.pop();
        assert!(
            fixture
                .prepare()
                .err()
                .unwrap()
                .to_string()
                .contains("no longer permitted")
        );
    }

    #[test]
    fn reuse_rejects_invalid_missing_or_unavailable_sessions() {
        let mut fixture = ReuseFixture::new();
        for id in [
            "../../outside".to_string(),
            fixture.session.id[..8].to_string(),
            uuid::Uuid::new_v4().to_string(),
        ] {
            fixture.args.session_id = Some(id);
            assert!(fixture.prepare().is_err());
        }
        fixture.args.session_id = Some(fixture.session.id.to_string());
        std::fs::remove_dir(fixture.session.working_dir.as_str()).unwrap();
        assert!(
            fixture
                .prepare()
                .err()
                .unwrap()
                .to_string()
                .contains("workspace is unavailable")
        );
    }

    #[test]
    fn fresh_tasks_still_create_a_new_session_in_the_current_workspace() {
        let mut fixture = ReuseFixture::new();
        fixture.args.session_id = None;
        let first = fixture.prepare().unwrap();
        let second = fixture.prepare().unwrap();
        assert_ne!(first.session_id, second.session_id);
        assert_ne!(first.session_id, fixture.session.id.as_str());
        assert_eq!(first.workspace, fixture.root.canonicalize().unwrap());
        assert_eq!(first.model_name, "new-default");
    }
}
