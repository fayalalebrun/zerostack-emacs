use crate::agent::tools::crc::crc32_hex;
use crate::agent::tools::set_edit_system;
use crate::agent::tools::{EditArgs, EditOp, edit};
use crate::config::types::EditSystem;
use rig::tool::Tool;

/// Serialize display artifact assertions because the artifact slot is global.
static EDIT_SYSTEM_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[must_use]
fn serialize_edit_system(es: EditSystem) -> std::sync::MutexGuard<'static, ()> {
    let guard = EDIT_SYSTEM_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    set_edit_system(es);
    guard
}

struct TempFile(String);

impl TempFile {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir()
            .join(format!("zerostack_test_{}_{}", name, uuid::Uuid::new_v4()))
            .to_string_lossy()
            .to_string();
        TempFile(path)
    }

    fn path(&self) -> &str {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn test_edit_records_display_patch_artifact() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("patch_artifact.txt");
    std::fs::write(tmp.path(), "before\n").unwrap();
    let tool = edit::EditTool::new(None, None);

    tool.call(
        &mut rig::tool::ToolContext::new(),
        EditArgs {
            content: None,
            path: tmp.path().into(),
            file_crc: Some(crc32_hex(b"before\n")),
            edits: Some(vec![EditOp {
                operation: Default::default(),
                line: Some(make_tagged_line(1, "before")),
                lines: None,
                text: "after".into(),
            }]),
        },
    )
    .await
    .unwrap();
    let artifact = edit::take_last_edit_display_artifact().unwrap();

    assert_eq!(artifact.kind, "patch");
    assert_eq!(artifact.extension, "patch");
    assert!(artifact.contents.contains("--- a/"));
    assert!(artifact.contents.contains("-before"));
    assert!(artifact.contents.contains("+after"));
}

#[tokio::test]
async fn test_edit_display_patch_uses_edit_hunk_not_whole_file() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("small_patch_artifact.txt");
    let content = (1..=80)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(tmp.path(), &content).unwrap();
    let tool = edit::EditTool::new(None, None);

    tool.call(
        &mut rig::tool::ToolContext::new(),
        EditArgs {
            content: None,
            path: tmp.path().into(),
            file_crc: Some(crc32_hex(content.as_bytes())),
            edits: Some(vec![EditOp {
                operation: Default::default(),
                line: Some(make_tagged_line(40, "line 40")),
                lines: None,
                text: "line forty".into(),
            }]),
        },
    )
    .await
    .unwrap();
    let artifact = edit::take_last_edit_display_artifact().unwrap();

    assert!(artifact.contents.contains("-line 40"));
    assert!(artifact.contents.contains("+line forty"));
    assert!(artifact.contents.contains(" line 37"));
    assert!(artifact.contents.contains(" line 43"));
    assert!(!artifact.contents.contains(" line 1"));
    assert!(!artifact.contents.contains(" line 80"));
}

fn make_tagged_line(line_num: usize, content: &str) -> String {
    let tag = crc32_hex(content.as_bytes());
    format!("   {}|{} {}", line_num, tag, content)
}

#[tokio::test]
async fn test_hash_single_line_edit() {
    let _edit_guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("hash_single.txt");
    let original = "use std::io;\nuse std::fs;\n\nfn main() {\n    println!(\"hi\");\n}\n";
    std::fs::write(tmp.path(), original).unwrap();
    let file_crc = crc32_hex(original.as_bytes());

    let tool = edit::EditTool::new(None, None);
    let tagged = make_tagged_line(4, "fn main() {");
    let result = tool
        .call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some(file_crc),
                edits: Some(vec![EditOp {
                    operation: Default::default(),
                    line: Some(tagged),
                    lines: None,
                    text: "fn run() {".into(),
                }]),
            },
        )
        .await
        .unwrap();

    let content = std::fs::read_to_string(tmp.path()).unwrap();
    assert!(
        content.contains("fn run() {"),
        "expected 'fn run() {{', got: {content}"
    );
    assert!(!content.contains("fn main() {"));
    assert!(result.contains("Applied 1 edit(s)"));
}

#[tokio::test]
async fn test_hash_range_edit() {
    let _edit_guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("hash_range.txt");
    let original = "line1\nline2\nline3\nline4\nline5\n";
    std::fs::write(tmp.path(), original).unwrap();
    let file_crc = crc32_hex(original.as_bytes());

    let tool = edit::EditTool::new(None, None);
    let l2 = make_tagged_line(2, "line2");
    let l3 = make_tagged_line(3, "line3");
    let l4 = make_tagged_line(4, "line4");
    let result = tool
        .call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some(file_crc),
                edits: Some(vec![EditOp {
                    operation: Default::default(),
                    line: None,
                    lines: Some(format!("{}\n{}\n{}", l2, l3, l4)),
                    text: "CHANGED_A\nCHANGED_B".into(),
                }]),
            },
        )
        .await
        .unwrap();

    let content = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(content, "line1\nCHANGED_A\nCHANGED_B\nline5\n");
    assert!(result.contains("Applied 1 edit(s)"));
}

#[tokio::test]
async fn test_hash_delete_via_empty_text() {
    let _edit_guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("hash_delete.txt");
    let original = "keep me\nremove me\nkeep me too\n";
    std::fs::write(tmp.path(), original).unwrap();
    let file_crc = crc32_hex(original.as_bytes());

    let tool = edit::EditTool::new(None, None);
    let tagged = make_tagged_line(2, "remove me");
    tool.call(
        &mut rig::tool::ToolContext::new(),
        EditArgs {
            content: None,
            path: tmp.path().into(),
            file_crc: Some(file_crc),
            edits: Some(vec![EditOp {
                operation: Default::default(),
                line: Some(tagged),
                lines: None,
                text: String::new(),
            }]),
        },
    )
    .await
    .unwrap();

    let content = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(content, "keep me\n\nkeep me too\n");
}

#[tokio::test]
async fn test_hash_file_crc_mismatch() {
    let _edit_guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("hash_badcrc.txt");
    std::fs::write(tmp.path(), "hello world\n").unwrap();

    let tool = edit::EditTool::new(None, None);
    let tagged = make_tagged_line(1, "hello world");
    let result = tool
        .call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some("deadbeef".into()),
                edits: Some(vec![EditOp {
                    operation: Default::default(),
                    line: Some(tagged),
                    lines: None,
                    text: "bye".into(),
                }]),
            },
        )
        .await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("CRC mismatch"));
}

#[tokio::test]
async fn test_hash_tag_mismatch() {
    let _edit_guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("hash_badtag.txt");
    let original = "hello world\n";
    std::fs::write(tmp.path(), original).unwrap();
    let file_crc = crc32_hex(original.as_bytes());

    let tool = edit::EditTool::new(None, None);
    // Tag is for "different content" not for "hello world"
    let bad_tag = crc32_hex(b"different content");
    let result = tool
        .call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some(file_crc),
                edits: Some(vec![EditOp {
                    operation: Default::default(),
                    line: Some(format!("   1|{} hello world", bad_tag)),
                    lines: None,
                    text: "bye".into(),
                }]),
            },
        )
        .await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("Tag mismatch"));
}

#[tokio::test]
async fn test_hash_invalid_tag_format() {
    let _edit_guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("hash_badfmt.txt");
    let original = "hello world\n";
    std::fs::write(tmp.path(), original).unwrap();
    let file_crc = crc32_hex(original.as_bytes());

    let tool = edit::EditTool::new(None, None);
    let result = tool
        .call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some(file_crc),
                edits: Some(vec![EditOp {
                    operation: Default::default(),
                    line: Some("not a valid tagged line".into()),
                    lines: None,
                    text: "bye".into(),
                }]),
            },
        )
        .await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("invalid tagged line"));
}

#[tokio::test]
async fn test_hash_crlf_preserved() {
    let _edit_guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("hash_crlf.txt");
    let original = "line1\r\nline2\r\nline3\r\n";
    std::fs::write(tmp.path(), original).unwrap();
    // CRC must be computed on LF-normalized content, same as edit tool normalizes
    let normalized = original.replace("\r\n", "\n");
    let file_crc = crc32_hex(normalized.as_bytes());

    let tool = edit::EditTool::new(None, None);
    let tagged = make_tagged_line(2, "line2");
    tool.call(
        &mut rig::tool::ToolContext::new(),
        EditArgs {
            content: None,
            path: tmp.path().into(),
            file_crc: Some(file_crc),
            edits: Some(vec![EditOp {
                operation: Default::default(),
                line: Some(tagged),
                lines: None,
                text: "modified".into(),
            }]),
        },
    )
    .await
    .unwrap();

    let raw = std::fs::read(tmp.path()).unwrap();
    assert!(
        raw.windows(2).any(|w| w == b"\r\n"),
        "CRLF should be preserved"
    );
}

#[tokio::test]
async fn test_hash_multi_edit_atomic() {
    let _edit_guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("hash_multi.txt");
    let original = "aaa\nbbb\nccc\nddd\n";
    std::fs::write(tmp.path(), original).unwrap();
    let file_crc = crc32_hex(original.as_bytes());

    let tool = edit::EditTool::new(None, None);
    let l1 = make_tagged_line(1, "aaa");
    let l4 = make_tagged_line(4, "ddd");
    let result = tool
        .call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some(file_crc),
                edits: Some(vec![
                    EditOp {
                        operation: Default::default(),
                        line: Some(l1),
                        lines: None,
                        text: "AAA".into(),
                    },
                    EditOp {
                        operation: Default::default(),
                        line: Some(l4),
                        lines: None,
                        text: "DDD".into(),
                    },
                ]),
            },
        )
        .await
        .unwrap();

    let content = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(content, "AAA\nbbb\nccc\nDDD\n");
    assert!(result.contains("Applied 2 edit(s)"));
}

#[test]
fn hashedit_is_default_and_legacy_config_migrates() {
    assert_eq!(EditSystem::default(), EditSystem::Hashedit);
    assert_eq!(
        "similarity".parse::<EditSystem>().unwrap(),
        EditSystem::Hashedit
    );
    assert_eq!(
        serde_json::from_str::<EditSystem>("\"similarity\"").unwrap(),
        EditSystem::Hashedit
    );
    assert_eq!(
        serde_json::to_string(&EditSystem::default()).unwrap(),
        "\"hashedit\""
    );
}

#[tokio::test]
async fn compact_references_read_edit_and_return_fresh_revision() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("compact_roundtrip.txt");
    std::fs::write(tmp.path(), "one\ntwo\nthree\nfour\n").unwrap();
    let read = crate::agent::tools::ReadTool::new(None, None, None, 2000);
    let output = read
        .call(
            &mut rig::tool::ToolContext::new(),
            crate::agent::tools::ReadArgs {
                path: tmp.path().into(),
                offset: None,
                limit: None,
            },
        )
        .await
        .unwrap()
        .into_content();
    let rig::message::ToolResultContent::Text(text) = output.first().unwrap() else {
        panic!("expected text");
    };
    let crc = text
        .text
        .split("[CRC: ")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    let references: Vec<_> = text
        .text
        .lines()
        .skip(2)
        .take(4)
        .map(|line| line.trim_start().split_once(' ').unwrap().0.to_string())
        .collect();
    let tool = edit::EditTool::new(None, None);
    let result = tool
        .call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some(crc.into()),
                edits: Some(vec![EditOp {
                    operation: Default::default(),
                    line: None,
                    lines: Some(format!("{}\n{}", references[1], references[3])),
                    text: "updated".into(),
                }]),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path()).unwrap(),
        "one\nupdated\n"
    );
    let new_crc = crc32_hex(b"one\nupdated\n");
    assert!(result.contains(&format!("[CRC: {new_crc}]")));
    let result = tool
        .call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some(new_crc),
                edits: Some(vec![EditOp {
                    operation: Default::default(),
                    line: Some(format!("2|{}", crc32_hex(b"updated"))),
                    lines: None,
                    text: "final".into(),
                }]),
            },
        )
        .await
        .unwrap();
    assert!(result.contains("Applied 1 edit(s)"));
    assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "one\nfinal\n");
}

#[tokio::test]
async fn invalid_hash_ranges_leave_file_unchanged() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("invalid_ranges.txt");
    let original = "one\ntwo\nthree\nfour\n";
    std::fs::write(tmp.path(), original).unwrap();
    let tool = edit::EditTool::new(None, None);
    let reference = |n, text: &str| format!("{n}|{}", crc32_hex(text.as_bytes()));
    for edits in [
        vec![EditOp {
            operation: Default::default(),
            line: Some(reference(0, "one")),
            lines: None,
            text: "bad".into(),
        }],
        vec![EditOp {
            operation: Default::default(),
            line: None,
            lines: Some(format!(
                "{}\n{}",
                reference(3, "three"),
                reference(1, "one")
            )),
            text: "bad".into(),
        }],
        vec![EditOp {
            operation: Default::default(),
            line: None,
            lines: Some(format!(
                "{}\n{}\n{}",
                reference(1, "one"),
                reference(3, "three"),
                reference(4, "four")
            )),
            text: "bad".into(),
        }],
        vec![
            EditOp {
                operation: Default::default(),
                line: None,
                lines: Some(format!(
                    "{}\n{}",
                    reference(1, "one"),
                    reference(3, "three")
                )),
                text: "bad".into(),
            },
            EditOp {
                operation: Default::default(),
                line: Some(reference(2, "two")),
                lines: None,
                text: "bad".into(),
            },
        ],
        vec![],
    ] {
        assert!(
            tool.call(
                &mut rig::tool::ToolContext::new(),
                EditArgs {
                    content: None,
                    path: tmp.path().into(),
                    file_crc: Some(crc32_hex(original.as_bytes())),
                    edits: Some(edits)
                }
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), original);
    }
}

#[tokio::test]
async fn non_utf8_edit_is_rejected_without_changing_bytes() {
    let tmp = TempFile::new("non_utf8.txt");
    let bytes = [0xff, b'\n'];
    std::fs::write(tmp.path(), bytes).unwrap();
    let tool = edit::EditTool::new(None, None);
    assert!(
        tool.call(
            &mut rig::tool::ToolContext::new(),
            EditArgs {
                content: None,
                path: tmp.path().into(),
                file_crc: Some(crc32_hex(&bytes)),
                edits: Some(vec![])
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("UTF-8")
    );
    assert_eq!(std::fs::read(tmp.path()).unwrap(), bytes);
}

async fn apply_json(
    path: &str,
    crc: &str,
    edits: serde_json::Value,
) -> Result<String, crate::agent::tools::ToolError> {
    let args =
        serde_json::from_value(serde_json::json!({"path": path, "file_crc": crc, "edits": edits}))
            .unwrap();
    edit::EditTool::new(None, None)
        .call(&mut rig::tool::ToolContext::new(), args)
        .await
}

fn reference(number: usize, text: &str) -> String {
    format!("{number}|{}", crc32_hex(text.as_bytes()))
}

#[tokio::test]
async fn explicit_insertions_and_deletions_preserve_surrounding_bytes() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let cases = [
        (
            "one\r\ntwo\nlast",
            serde_json::json!({"operation":"insert_before", "line": reference(2,"two"), "text":"new"}),
            "one\r\nnew\ntwo\nlast",
        ),
        (
            "one\r\ntwo\nlast",
            serde_json::json!({"operation":"insert_after", "line": reference(1,"one"), "text":"new"}),
            "one\r\nnew\r\ntwo\nlast",
        ),
        (
            "one\r\ntwo\nlast",
            serde_json::json!({"operation":"delete", "text":"", "line": reference(2,"two")}),
            "one\r\nlast",
        ),
        (
            "one\ntwo\nlast",
            serde_json::json!({"operation":"delete", "text":"", "line": reference(3,"last")}),
            "one\ntwo\n",
        ),
        (
            "one\ntwo\nlast",
            serde_json::json!({"operation":"insert_after", "line": reference(3,"last"), "text":"new"}),
            "one\ntwo\nlast\nnew\n",
        ),
        (
            "one\ntwo\nlast",
            serde_json::json!({"operation":"append", "text":"new\n\n"}),
            "one\ntwo\nlast\nnew\n\n",
        ),
        (
            "",
            serde_json::json!({"operation":"append", "text":"first"}),
            "first\n",
        ),
        (
            "only\n",
            serde_json::json!({"operation":"delete", "text":"", "line": reference(1,"only")}),
            "",
        ),
        (
            "\r\n",
            serde_json::json!({"operation":"replace", "line": reference(1,""), "text":"filled"}),
            "filled\r\n",
        ),
        (
            "é🦀\r\nunchanged\nlast",
            serde_json::json!({"line": reference(1,"é🦀"), "text":"你好\nnew"}),
            "你好\r\nnew\r\nunchanged\nlast",
        ),
    ];
    for (original, op, expected) in cases {
        let tmp = TempFile::new("operations");
        std::fs::write(tmp.path(), original).unwrap();
        let result = apply_json(
            tmp.path(),
            &crc32_hex(original.replace("\r\n", "\n").as_bytes()),
            serde_json::json!([op]),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), expected);
        assert!(result.contains(&format!(
            "[CRC: {}]",
            crc32_hex(expected.replace("\r\n", "\n").as_bytes())
        )));
    }
}

#[tokio::test]
async fn mixed_line_endings_and_missing_final_newline_survive_targeted_edits() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("mixed");
    let original = "first\r\nsecond\nthird\r\nlast";
    std::fs::write(tmp.path(), original).unwrap();
    let result = apply_json(
        tmp.path(),
        &crc32_hex(original.replace("\r\n", "\n").as_bytes()),
        serde_json::json!([
            {"line":reference(1,"first"),"text":"changed"},
            {"line":reference(4,"last"),"text":"final"}
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path()).unwrap(),
        "changed\r\nsecond\nthird\r\nfinal"
    );
    assert!(result.contains(&reference(4, "final")));
}

#[tokio::test]
async fn noop_edit_returns_revision_without_replacing_file() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("noop");
    std::fs::write(tmp.path(), "same\r\n").unwrap();
    let before = std::fs::metadata(tmp.path()).unwrap();
    let result = apply_json(
        tmp.path(),
        &crc32_hex(b"same\n"),
        serde_json::json!([{"line":reference(1,"same"),"text":"same"}]),
    )
    .await
    .unwrap();
    assert!(result.starts_with("No changes"));
    assert_eq!(
        before.modified().unwrap(),
        std::fs::metadata(tmp.path()).unwrap().modified().unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(before.ino(), std::fs::metadata(tmp.path()).unwrap().ino());
    }
}

#[tokio::test]
async fn guarded_whole_file_replacement_is_exact_and_rejects_stale_revision() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("whole");
    std::fs::write(tmp.path(), "old\r\n").unwrap();
    let tool = edit::EditTool::new(None, None);
    let args = |crc: &str| {
        serde_json::from_value(
            serde_json::json!({"path":tmp.path(),"file_crc":crc,"content":"new\nexact\r\n"}),
        )
        .unwrap()
    };
    assert!(
        tool.call(&mut rig::tool::ToolContext::new(), args("00000000"))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "old\r\n");
    tool.call(
        &mut rig::tool::ToolContext::new(),
        args(&crc32_hex(b"old\n")),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path()).unwrap(),
        "new\nexact\r\n"
    );
    let args = serde_json::from_value(serde_json::json!({"path":tmp.path(),"file_crc":crc32_hex(b"new\nexact\n"),"content":"bad", "edits":[]})).unwrap();
    assert!(
        tool.call(&mut rig::tool::ToolContext::new(), args)
            .await
            .is_err()
    );
    assert!(
        serde_json::from_value::<EditArgs>(
            serde_json::json!({"path":tmp.path(), "block":"old format"})
        )
        .is_err()
    );
}

#[tokio::test]
async fn conflicting_operations_and_insertion_points_are_rejected_atomically() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("conflicts");
    let source = "\nsecond\nthird\n";
    std::fs::write(tmp.path(), source).unwrap();
    for edits in [
        serde_json::json!([{"line":reference(1,""),"text":"a"},{"line":reference(1,""),"text":"b"}]),
        serde_json::json!([{"operation":"append","text":"a"},{"operation":"append","text":"b"}]),
        serde_json::json!([{"operation":"delete","line":reference(2,"second"),"text":"not allowed"}]),
        serde_json::json!([{"operation":"insert_before","lines":format!("{}\n{}",reference(2,"second"),reference(3,"third")),"text":"bad"}]),
        serde_json::json!([{"operation":"append","line":reference(1,""),"text":"bad"}]),
    ] {
        assert!(
            apply_json(tmp.path(), &crc32_hex(source.as_bytes()), edits)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), source);
    }
}

#[tokio::test]
async fn concurrent_edits_from_same_revision_cannot_lose_an_update() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("concurrent");
    std::fs::write(tmp.path(), "old\n").unwrap();
    let crc = crc32_hex(b"old\n");
    let left = apply_json(
        tmp.path(),
        &crc,
        serde_json::json!([{"line":reference(1,"old"),"text":"left"}]),
    );
    let right = apply_json(
        tmp.path(),
        &crc,
        serde_json::json!([{"line":reference(1,"old"),"text":"right"}]),
    );
    let (left, right) = tokio::join!(left, right);
    assert_ne!(left.is_ok(), right.is_ok());
    let result = std::fs::read_to_string(tmp.path()).unwrap();
    assert!(result == "left\n" || result == "right\n");
}

#[tokio::test]
async fn replacement_size_limit_is_checked_before_mutation() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let tmp = TempFile::new("size");
    std::fs::write(tmp.path(), "old\n").unwrap();
    let tool = edit::EditTool::new(None, None).with_max_text_file_size(Some(3));
    for args in [
        serde_json::json!({"path":tmp.path(),"file_crc":crc32_hex(b"old\n"),"content":"large"}),
        serde_json::json!({"path":tmp.path(),"file_crc":crc32_hex(b"old\n"),"edits":[{"line":reference(1,"old"),"text":"large"}]}),
    ] {
        assert!(
            tool.call(
                &mut rig::tool::ToolContext::new(),
                serde_json::from_value(args).unwrap()
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "old\n");
    }
}

#[tokio::test]
async fn insertion_at_replacement_start_can_batch_with_range_edit_and_deletion() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let original = "alpha\nbeta\ngamma\ndelta\n";
    let expected = "alpha\ninserted\nBETA\nGAMMA\n";
    let operations = [
        serde_json::json!({"operation":"insert_after", "line":reference(1,"alpha"), "text":"inserted"}),
        serde_json::json!({"lines":format!("{}\n{}", reference(2,"beta"), reference(3,"gamma")), "text":"BETA\nGAMMA"}),
        serde_json::json!({"operation":"delete", "line":reference(4,"delta"), "text":""}),
    ];
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let tmp = TempFile::new("boundary_batch");
        std::fs::write(tmp.path(), original).unwrap();
        let edits = order.map(|index| operations[index].clone());
        let result = apply_json(
            tmp.path(),
            &crc32_hex(original.as_bytes()),
            serde_json::json!(edits),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), expected);
        assert!(result.contains("Applied 3 edit(s)"));
        assert!(result.contains(&format!("[CRC: {}]", crc32_hex(expected.as_bytes()))));
        assert!(result.contains(&reference(2, "inserted")));
        assert!(result.contains(&reference(3, "BETA")));
        assert!(result.contains(&reference(4, "GAMMA")));
    }
}

#[tokio::test]
async fn boundary_insertions_have_defined_order_for_replacements_and_deletions() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let cases = [
        (
            "alpha\nbeta\ngamma\n",
            "alpha\ninserted\nBETA\ngamma\n",
            serde_json::json!({"operation":"insert_before", "line":reference(2,"beta"), "text":"inserted"}),
            serde_json::json!({"line":reference(2,"beta"), "text":"BETA"}),
        ),
        (
            "alpha\nbeta\ngamma\n",
            "alpha\ninserted\ngamma\n",
            serde_json::json!({"operation":"insert_before", "line":reference(2,"beta"), "text":"inserted"}),
            serde_json::json!({"operation":"delete", "line":reference(2,"beta"), "text":""}),
        ),
        (
            "alpha\nbeta\ngamma\n",
            "alpha\ninserted\ngamma\n",
            serde_json::json!({"operation":"insert_before", "line":reference(3,"gamma"), "text":"inserted"}),
            serde_json::json!({"operation":"delete", "line":reference(2,"beta"), "text":""}),
        ),
        (
            "alpha\nbeta",
            "alpha\nBETA\ntail\n",
            serde_json::json!({"operation":"append", "text":"tail"}),
            serde_json::json!({"line":reference(2,"beta"), "text":"BETA"}),
        ),
        (
            "\nlast\n",
            "inserted\nFILLED\nlast\n",
            serde_json::json!({"operation":"insert_before", "line":reference(1,""), "text":"inserted"}),
            serde_json::json!({"line":reference(1,""), "text":"FILLED"}),
        ),
        (
            "alpha\r\nbeta\r\ngamma\n",
            "alpha\r\ninserted\r\nBETA\r\ngamma\n",
            serde_json::json!({"operation":"insert_after", "line":reference(1,"alpha"), "text":"inserted"}),
            serde_json::json!({"line":reference(2,"beta"), "text":"BETA"}),
        ),
    ];
    for (original, expected, insertion, modification) in cases {
        for edits in [
            serde_json::json!([insertion, modification]),
            serde_json::json!([modification, insertion]),
        ] {
            let tmp = TempFile::new("boundary_order");
            std::fs::write(tmp.path(), original).unwrap();
            let revision = crc32_hex(original.replace("\r\n", "\n").as_bytes());
            apply_json(tmp.path(), &revision, edits).await.unwrap();
            assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), expected);
        }
    }
}

#[tokio::test]
async fn interior_insertions_and_duplicate_boundary_insertions_still_fail_atomically() {
    let _guard = serialize_edit_system(EditSystem::Hashedit);
    let original = "alpha\nbeta\ngamma\ndelta\n";
    let range = format!("{}\n{}", reference(2, "beta"), reference(3, "gamma"));
    for modification in [
        serde_json::json!({"lines":range, "text":"replacement"}),
        serde_json::json!({"operation":"delete", "lines":range, "text":""}),
    ] {
        let invalid_batches = [
            serde_json::json!([modification, {"operation":"insert_before", "line":reference(3,"gamma"), "text":"inside"}]),
            serde_json::json!([modification, {"operation":"insert_before", "line":reference(2,"beta"), "text":"first"}, {"operation":"insert_after", "line":reference(1,"alpha"), "text":"second"}]),
        ];
        for batch in invalid_batches {
            for reverse in [false, true] {
                let tmp = TempFile::new("boundary_conflict");
                std::fs::write(tmp.path(), original).unwrap();
                let mut edits = batch.as_array().unwrap().clone();
                if reverse {
                    edits.reverse();
                }
                assert!(
                    apply_json(
                        tmp.path(),
                        &crc32_hex(original.as_bytes()),
                        serde_json::json!(edits)
                    )
                    .await
                    .is_err()
                );
                assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), original);
            }
        }
    }
}
