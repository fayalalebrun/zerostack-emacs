use super::*;
use crate::config::ServiceTier;

struct TestData {
    root: PathBuf,
    previous: Option<PathBuf>,
}

impl TestData {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("zerostack-fork-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let previous = storage::set_test_data_dir(Some(root.clone()));
        Self { root, previous }
    }
}

impl Drop for TestData {
    fn drop(&mut self) {
        storage::set_test_data_dir(self.previous.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn attachment(name: &str) -> SessionAttachment {
    SessionAttachment {
        filename: name.into(),
        stored_name: name.into(),
        mime: "image/png".into(),
        size_bytes: 5,
    }
}

fn load(session: &Session) -> Session {
    serde_json::from_slice(&std::fs::read(storage::session_path(&session.id)).unwrap()).unwrap()
}

#[test]
fn fork_preserves_history_settings_and_directory_with_independent_identity_and_accounting() {
    let mut source = Session::new("codex", "gpt-5.5", 128000);
    source.service_tier = Some(ServiceTier::Fast);
    source.reasoning_effort = Some("high".into());
    source.reasoning_enabled = false;
    source.input_token_cost = 2.0;
    source.working_dir = "/same/project".into();
    source.parent_session_id = Some("old-parent".into());
    source.subagent_access = Some("write".into());
    source
        .permission_allowlist
        .push(crate::session::PermissionAllowEntry {
            tool: "bash".into(),
            pattern: "*".into(),
        });
    source.goal = Some(
        serde_json::from_value(serde_json::json!({
            "content":"original goal", "status":"in_progress", "priority":"high"
        }))
        .unwrap(),
    );
    source.add_message(MessageRole::User, "first prompt");
    source.add_message(MessageRole::Assistant, "first answer");
    source.add_message(MessageRole::User, "second prompt");
    source.add_message(MessageRole::Assistant, "second answer");
    source.total_input_tokens = 500;
    source.total_output_tokens = 100;
    source.total_cost = 1.25;
    source.set_calibration(500, 100);
    source.add_provider_call(0, crate::event::TokenUsage::default(), 123);
    source.rewind_to(3);
    let original = serde_json::to_value(&source).unwrap();
    let fork = source.fork(None).unwrap();
    assert_ne!(fork.id, source.id);
    assert_eq!(fork.fork_source.as_ref().unwrap().session_id, source.id);
    assert_eq!(fork.fork_source.as_ref().unwrap().message_index, 3);
    assert_eq!(
        serde_json::to_value(&fork.messages).unwrap(),
        original["messages"]
    );
    assert_eq!(fork.provider, source.provider);
    assert_eq!(fork.model, source.model);
    assert_eq!(fork.service_tier, Some(ServiceTier::Fast));
    assert_eq!(fork.reasoning_effort, source.reasoning_effort);
    assert_eq!(fork.reasoning_enabled, source.reasoning_enabled);
    assert_eq!(fork.context_window, source.context_window);
    assert_eq!(fork.input_token_cost, source.input_token_cost);
    assert_eq!(fork.working_dir, source.working_dir);
    assert!(fork.parent_session_id.is_none());
    assert!(fork.subagent_access.is_none());
    assert!(fork.goal.is_none());
    assert!(fork.permission_allowlist.is_empty());
    assert!(fork.rewind_undo.is_none());
    assert!(fork.provider_calls.is_empty());
    assert_eq!(fork.total_cost, 0.0);
    assert_eq!(fork.total_input_tokens + fork.total_output_tokens, 0);
    assert_eq!(fork.calibrated_tokens, 0);
    assert_eq!(serde_json::to_value(&source).unwrap(), original);
    assert!(source.rewind_undo.is_some());
}

#[test]
fn indexed_forks_stop_before_user_messages_and_reject_other_boundaries() {
    let mut source = Session::new("test", "test", 1000);
    source.add_message(MessageRole::User, "first");
    source.add_message(MessageRole::Assistant, "answer");
    source.add_message(MessageRole::User, "change this");
    assert!(source.fork(Some(0)).unwrap().messages.is_empty());
    let fork = source.fork(Some(2)).unwrap();
    assert_eq!(fork.messages.len(), 2);
    assert_eq!(
        fork.total_estimated_tokens,
        fork.messages
            .iter()
            .map(|m| m.estimated_tokens)
            .sum::<u64>()
    );
    for index in [1, 3, usize::MAX] {
        assert!(source.fork(Some(index)).is_err());
    }
    assert_eq!(source.messages.len(), 3);
}

#[test]
fn fork_retains_compacted_context_without_changing_the_source() {
    let mut source = Session::new("test", "test", 1000);
    source.add_message(MessageRole::User, "old");
    source.add_message(MessageRole::Assistant, "answer");
    source.compress("summary".to_string(), 0, 0);
    source.add_message(MessageRole::User, "new prompt");
    let fork = source.fork(Some(source.messages.len() - 1)).unwrap();
    assert_eq!(fork.compacted_context().0, source.compacted_context().0);
    assert_eq!(fork.compactions.len(), source.compactions.len());
}

#[test]
fn saved_fork_owns_user_media_tool_media_outputs_and_display_artifacts() {
    let _data = TestData::new();
    let mut source = Session::new("test", "test", 1000);
    source.add_message(MessageRole::User, "look at this");
    source.messages[0].attachments.push(attachment("user.png"));
    let media = storage::media_dir(&source.id);
    std::fs::create_dir_all(&media).unwrap();
    for name in ["user.png", "tool.png"] {
        std::fs::write(media.join(name), b"image").unwrap();
    }
    source.add_tool_result(
        "bash",
        &"x".repeat(crate::session::TOOL_RESULT_SAVE_THRESHOLD + 1),
    );
    let outputs = storage::tool_output_dir(&source.id);
    let display = outputs.join("changes.patch");
    std::fs::write(&display, b"patch").unwrap();
    let result = source.messages[1].tool_result.as_mut().unwrap();
    result.attachments.push(attachment("tool.png"));
    result.display_path = Some(display.to_string_lossy().into_owned().into());
    let old_output = result.output_path.clone().unwrap();
    // The same asset also appears in legacy unstructured tool result text.
    source.add_message(
        MessageRole::ToolResult,
        &format!("[full output saved to: {old_output}; use the read tool]"),
    );
    storage::save_session(&source).unwrap();
    let original_json = std::fs::read(storage::session_path(&source.id)).unwrap();
    let fork = save(&source, None).unwrap();
    assert_eq!(
        std::fs::read(storage::session_path(&source.id)).unwrap(),
        original_json
    );
    assert!(load(&fork).goal.is_none());
    storage::delete_session(&source.id).unwrap();
    let fork = load(&fork);
    for name in ["user.png", "tool.png"] {
        assert_eq!(
            std::fs::read(storage::media_dir(&fork.id).join(name)).unwrap(),
            b"image"
        );
    }
    let result = fork.messages[1].tool_result.as_ref().unwrap();
    assert_eq!(
        std::fs::read_to_string(result.output_path.as_ref().unwrap().as_str()).unwrap(),
        "x".repeat(crate::session::TOOL_RESULT_SAVE_THRESHOLD + 1)
    );
    assert_eq!(
        std::fs::read(result.display_path.as_ref().unwrap().as_str()).unwrap(),
        b"patch"
    );
    assert!(!fork.messages[2].content.contains(old_output.as_str()));
    assert!(
        fork.messages[2]
            .content
            .contains(result.output_path.as_ref().unwrap().as_str())
    );
}

#[test]
fn indexed_save_does_not_copy_assets_from_excluded_messages() {
    let _data = TestData::new();
    let mut source = Session::new("test", "test", 1000);
    source.add_message(MessageRole::User, "editable draft");
    source.add_message(MessageRole::User, "excluded");
    source.messages[1]
        .attachments
        .push(attachment("missing.png"));
    let fork = save(&source, Some(0)).unwrap();
    assert!(load(&fork).messages.is_empty());
    assert!(!storage::media_dir(&fork.id).exists());
}

#[test]
fn missing_assets_roll_back_new_directories_without_changing_original_files() {
    let data = TestData::new();
    let mut source = Session::new("test", "test", 1000);
    source.add_message(MessageRole::User, "prompt");
    source.messages[0].attachments = vec![attachment("good.png"), attachment("missing.png")];
    std::fs::create_dir_all(storage::media_dir(&source.id)).unwrap();
    std::fs::write(storage::media_dir(&source.id).join("good.png"), b"image").unwrap();
    storage::save_session(&source).unwrap();
    let original = std::fs::read(storage::session_path(&source.id)).unwrap();
    assert!(save(&source, None).is_err());
    assert_eq!(
        std::fs::read_dir(data.root.join("media")).unwrap().count(),
        1
    );
    assert_eq!(
        std::fs::read_dir(data.root.join("sessions"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read(storage::session_path(&source.id)).unwrap(),
        original
    );
}

#[test]
fn invalid_attachment_names_cannot_escape_session_storage() {
    let _data = TestData::new();
    let mut source = Session::new("test", "test", 1000);
    source.add_message(MessageRole::User, "prompt");
    for name in [
        "../private.png",
        "/tmp/private.png",
        ".",
        "nested/file.png",
        "",
    ] {
        source.messages[0].attachments = vec![attachment(name)];
        assert!(save(&source, None).is_err(), "{name}");
    }
}

#[cfg(unix)]
#[test]
fn asset_symlinks_cannot_copy_files_outside_the_source_session_directory() {
    let data = TestData::new();
    let mut source = Session::new("test", "test", 1000);
    source.add_message(MessageRole::User, "prompt");
    source.messages[0]
        .attachments
        .push(attachment("escape.png"));
    std::fs::write(data.root.join("private.png"), b"private").unwrap();
    let media = storage::media_dir(&source.id);
    std::fs::create_dir_all(&media).unwrap();
    std::os::unix::fs::symlink(data.root.join("private.png"), media.join("escape.png")).unwrap();
    assert!(save(&source, None).is_err());
    assert_eq!(
        std::fs::read_dir(data.root.join("media")).unwrap().count(),
        1
    );
}

#[test]
fn indexed_fork_copies_editable_draft_media_without_keeping_the_draft_in_history() {
    let _data = TestData::new();
    let mut source = Session::new("test", "test", 1000);
    source.add_message(MessageRole::User, "change this prompt");
    source.messages[0].attachments.push(attachment("draft.png"));
    std::fs::create_dir_all(storage::media_dir(&source.id)).unwrap();
    std::fs::write(storage::media_dir(&source.id).join("draft.png"), b"image").unwrap();
    let fork = save(&source, Some(0)).unwrap();
    assert!(load(&fork).messages.is_empty());
    storage::delete_session(&source.id).unwrap();
    assert_eq!(
        std::fs::read(storage::media_dir(&fork.id).join("draft.png")).unwrap(),
        b"image"
    );
}
