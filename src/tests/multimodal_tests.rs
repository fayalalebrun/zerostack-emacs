#![cfg(feature = "multimodal")]

use crate::extras::multimodal::{
    MediaAttachment, detect_media, load_attachment, load_persisted_attachment, persist_attachment,
};
use std::path::Path;

// --- detect_media tests ---

#[test]
fn detect_media_image_extensions() {
    assert_eq!(detect_media(Path::new("photo.png")), Some("image/png"));
    assert_eq!(detect_media(Path::new("photo.jpg")), Some("image/jpeg"));
    assert_eq!(detect_media(Path::new("photo.jpeg")), Some("image/jpeg"));
    assert_eq!(detect_media(Path::new("photo.GIF")), Some("image/gif"));
    assert_eq!(detect_media(Path::new("photo.webp")), Some("image/webp"));
}

#[test]
fn detect_media_audio_extensions() {
    assert_eq!(detect_media(Path::new("song.mp3")), Some("audio/mpeg"));
    assert_eq!(detect_media(Path::new("song.wav")), Some("audio/wav"));
    assert_eq!(detect_media(Path::new("song.ogg")), Some("audio/ogg"));
    assert_eq!(detect_media(Path::new("song.flac")), Some("audio/flac"));
    assert_eq!(detect_media(Path::new("song.m4a")), Some("audio/mp4"));
    assert_eq!(detect_media(Path::new("song.aac")), Some("audio/aac"));
}

#[test]
fn detect_media_document_extension() {
    assert_eq!(detect_media(Path::new("doc.pdf")), Some("application/pdf"));
}

#[test]
fn detect_media_unknown_returns_none() {
    assert_eq!(detect_media(Path::new("code.rs")), None);
    assert_eq!(detect_media(Path::new("README.md")), None);
    assert_eq!(detect_media(Path::new("script.sh")), None);
    assert_eq!(detect_media(Path::new("Dockerfile")), None);
    assert_eq!(detect_media(Path::new("data.txt")), None);
}

#[test]
fn detect_media_no_extension_returns_none() {
    assert_eq!(detect_media(Path::new("Makefile")), None);
    assert_eq!(detect_media(Path::new("/usr/bin/binary")), None);
}

// --- load_attachment tests ---

#[test]
fn load_attachment_file_not_found() {
    let err = load_attachment(Path::new("/nonexistent/file.png")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn load_attachment_unknown_media_type() {
    let err = load_attachment(Path::new("Cargo.toml")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn load_attachment_rejects_malformed_image() {
    let path = std::env::temp_dir().join(format!("zerostack-bad-{}.png", uuid::Uuid::new_v4()));
    std::fs::write(&path, b"\x89PNG\r\n\x1a\ninvalid").unwrap();

    let error = load_attachment(&path).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn load_attachment_success_for_small_media() {
    let dir = std::env::temp_dir();
    let path = dir.join("zerostack_test_media.png");
    let png = crate::extras::image_validate::test_png();
    std::fs::write(&path, &png).unwrap();
    let result = load_attachment(&path);
    let _ = std::fs::remove_file(&path);
    assert!(result.is_ok(), "expected Ok, got {result:?}");
    let att = result.unwrap();
    assert_eq!(att.size(), png.len());
    assert_eq!(att.path().to_string_lossy(), path.to_string_lossy());
}

// --- MediaAttachment size and path ---

#[test]
fn media_attachment_size_matches_data_len() {
    let att = MediaAttachment::Image {
        path: Path::new("test.png").to_path_buf(),
        data: vec![0u8; 42],
        mime: "image/png".into(),
    };
    assert_eq!(att.size(), 42);
}

#[test]
fn media_attachment_path_returns_stored_path() {
    let att = MediaAttachment::Audio {
        path: Path::new("/tmp/sound.wav").to_path_buf(),
        data: vec![0u8; 10],
        mime: "audio/wav".into(),
    };
    assert_eq!(att.path(), Path::new("/tmp/sound.wav"));
}

// --- media_to_messages tests ---

#[cfg(feature = "multimodal")]
#[test]
fn media_to_messages_produces_user_messages() {
    use crate::agent::runner::media_to_messages;
    use rig::completion::Message;
    use rig::completion::message::{DocumentSourceKind, UserContent};

    let media = vec![
        MediaAttachment::Image {
            path: Path::new("photo.png").to_path_buf(),
            data: vec![1, 2, 3],
            mime: "image/png".into(),
        },
        MediaAttachment::Document {
            path: Path::new("doc.pdf").to_path_buf(),
            data: vec![4, 5, 6],
            mime: "application/pdf".into(),
        },
    ];

    let messages = media_to_messages(&media);
    assert_eq!(messages.len(), 2);

    for msg in &messages {
        assert!(
            matches!(msg, Message::User { .. }),
            "expected User message, got {msg:?}"
        );
    }

    let Message::User { content } = &messages[0] else {
        unreachable!()
    };
    let UserContent::Image(image) = content.first_ref() else {
        panic!("expected image content")
    };
    assert!(matches!(image.data, DocumentSourceKind::Base64(_)));

    let Message::User { content } = &messages[1] else {
        unreachable!()
    };
    let UserContent::Document(document) = content.first_ref() else {
        panic!("expected document content")
    };
    assert!(matches!(document.data, DocumentSourceKind::Base64(_)));
}

#[cfg(feature = "multimodal")]
#[test]
fn media_to_messages_empty_vec_returns_empty() {
    use crate::agent::runner::media_to_messages;

    let messages = media_to_messages(&[]);
    assert!(messages.is_empty());
}

#[test]
fn persisted_attachment_survives_serialization_and_reloads_bytes() {
    let dir = std::env::temp_dir().join(format!("zerostack-media-test-{}", uuid::Uuid::new_v4()));
    let previous = crate::session::storage::set_test_data_dir(Some(dir.clone()));
    let png = crate::extras::image_validate::test_png();
    let media = MediaAttachment::Image {
        path: Path::new("clipboard.png").to_path_buf(),
        data: png.clone(),
        mime: "image/png".into(),
    };
    let attachment = persist_attachment("session-1", &media).unwrap();
    let encoded = serde_json::to_string(&attachment).unwrap();
    let decoded = serde_json::from_str(&encoded).unwrap();
    let loaded = load_persisted_attachment("session-1", &decoded).unwrap();

    assert_eq!(loaded.size(), png.len());
    assert_eq!(decoded.filename, "clipboard.png");
    crate::session::storage::set_test_data_dir(previous);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn convert_history_replays_persisted_attachment_with_its_turn() {
    use rig::completion::Message;
    use rig::completion::message::UserContent;

    let dir = std::env::temp_dir().join(format!("zerostack-media-test-{}", uuid::Uuid::new_v4()));
    let previous = crate::session::storage::set_test_data_dir(Some(dir.clone()));
    let media = MediaAttachment::Image {
        path: Path::new("clipboard.png").to_path_buf(),
        data: crate::extras::image_validate::test_png(),
        mime: "image/png".into(),
    };
    let attachment = persist_attachment("session-1", &media).unwrap();
    let mut session = crate::session::Session::new("openai", "model", 100_000);
    session.id = "session-1".into();
    session.add_message(crate::session::MessageRole::User, "What is this?");
    session.messages[0].attachments.push(attachment);

    let history = crate::agent::runner::convert_history(&session);
    assert_eq!(history.len(), 2);
    let Message::User { content } = &history[0] else {
        panic!("expected media user message")
    };
    assert!(matches!(content.first_ref(), UserContent::Image(_)));
    assert!(matches!(&history[1], Message::User { .. }));

    crate::session::storage::set_test_data_dir(previous);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn convert_history_replays_image_tool_result() {
    use rig::completion::Message;
    use rig::completion::message::UserContent;

    let dir = std::env::temp_dir().join(format!("zerostack-media-test-{}", uuid::Uuid::new_v4()));
    let previous = crate::session::storage::set_test_data_dir(Some(dir.clone()));
    let mut session = crate::session::Session::new("openai", "model", 100_000);
    session.id = "session-tool-image".into();
    session.add_tool_call_structured(
        "read",
        &serde_json::json!({"path": "image.png"}),
        "call-1",
        None,
    );
    session.add_tool_result_structured("read", "Read image: image.png", "call-1", None);
    let attachment = crate::extras::multimodal::persist_bytes(
        &session.id,
        "image.png",
        "image/png",
        &crate::extras::image_validate::test_png(),
    )
    .unwrap();
    session.messages[1]
        .tool_result
        .as_mut()
        .unwrap()
        .attachments
        .push(attachment);

    let history = crate::agent::runner::convert_history(&session);
    let Message::User { content } = &history[1] else {
        panic!("expected tool result user message")
    };
    assert!(matches!(content.first_ref(), UserContent::ToolResult(_)));
    let Message::User { content } = &history[2] else {
        panic!("expected separate image user message")
    };
    assert!(matches!(content.first_ref(), UserContent::Image(_)));

    crate::session::storage::set_test_data_dir(previous);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn corrupt_persisted_tool_image_replays_as_warning() {
    use rig::completion::Message;
    use rig::completion::message::{ToolResultContent, UserContent};

    let dir = std::env::temp_dir().join(format!("zerostack-media-test-{}", uuid::Uuid::new_v4()));
    let previous = crate::session::storage::set_test_data_dir(Some(dir.clone()));
    let mut session = crate::session::Session::new("openai", "model", 100_000);
    session.id = "session-corrupt-image".into();
    session.add_tool_call_structured(
        "read",
        &serde_json::json!({"path": "image.png"}),
        "call-1",
        None,
    );
    session.add_tool_result_structured("read", "Read image: image.png", "call-1", None);
    let attachment = crate::extras::multimodal::persist_bytes(
        &session.id,
        "image.png",
        "image/png",
        &crate::extras::image_validate::test_png(),
    )
    .unwrap();
    std::fs::write(
        crate::session::storage::media_dir(&session.id).join(&attachment.stored_name),
        b"corrupt",
    )
    .unwrap();
    session.messages[1]
        .tool_result
        .as_mut()
        .unwrap()
        .attachments
        .push(attachment);

    let history = crate::agent::runner::convert_history(&session);
    let Message::User { content } = history.last().unwrap() else {
        panic!("expected tool result")
    };
    let UserContent::ToolResult(result) = content.first_ref() else {
        panic!("expected tool result")
    };
    assert!(result.content.iter().any(|item| matches!(
        item,
        ToolResultContent::Text(text) if text.text.contains("failed to load image attachment")
    )));
    assert!(
        !result
            .content
            .iter()
            .any(|item| matches!(item, ToolResultContent::Image(_)))
    );

    crate::session::storage::set_test_data_dir(previous);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn persisted_attachment_rejects_path_traversal() {
    let attachment = crate::session::SessionAttachment {
        filename: "image.png".into(),
        stored_name: "../image.png".into(),
        mime: "image/png".into(),
        size_bytes: 1,
    };

    let error = load_persisted_attachment("session-1", &attachment).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}
