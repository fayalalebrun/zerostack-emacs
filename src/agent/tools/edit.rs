use rig::tool::Tool;

use crate::agent::tools::crc::{crc32_hex, crc32_text};
use crate::agent::tools::{
    AskSender, EditArgs, EditOp, EditOperation, PermCheck, ToolError, check_perm_path,
};
use crate::event::DisplayArtifact;

pub struct EditTool {
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
    pub max_text_file_size: u64,
}

impl EditTool {
    pub fn new(permission: Option<PermCheck>, ask_tx: Option<AskSender>) -> Self {
        EditTool {
            permission,
            ask_tx,
            max_text_file_size: 1024 * 1024,
        }
    }

    pub fn with_max_text_file_size(mut self, limit: Option<u64>) -> Self {
        if let Some(limit) = limit {
            self.max_text_file_size = limit;
        }
        self
    }
}

static LAST_EDIT_DISPLAY_ARTIFACT: std::sync::Mutex<Option<DisplayArtifact>> =
    std::sync::Mutex::new(None);

pub fn take_last_edit_display_artifact() -> Option<DisplayArtifact> {
    LAST_EDIT_DISPLAY_ARTIFACT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
}

fn set_last_edit_display_artifact(artifact: DisplayArtifact) {
    *LAST_EDIT_DISPLAY_ARTIFACT
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(artifact);
}

fn edit_ranges_diff(path: &str, old: &str, ranges: &[(usize, usize, String)]) -> String {
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    let lines = old.lines().collect::<Vec<_>>();
    let offsets = line_offsets(old);
    let mut ranges = ranges.to_vec();
    ranges.sort_by_key(|(start, _, _)| *start);

    for (byte_start, byte_end, replace) in ranges {
        let old_start = if byte_start == old.len() && (old.is_empty() || old.ends_with('\n')) {
            lines.len()
        } else {
            line_at_byte(&offsets, byte_start)
        };
        let old_end = if byte_end > byte_start {
            line_at_byte(&offsets, byte_end.saturating_sub(1)) + 1
        } else if byte_start > line_byte(&offsets, old_start, old.len()) {
            old_start + 1
        } else {
            old_start
        };
        let hunk_start = old_start.saturating_sub(3);
        let hunk_end = (old_end + 3).min(lines.len());
        let old_start_byte = line_byte(&offsets, old_start, old.len());
        let old_end_byte = line_byte(&offsets, old_end, old.len());
        let old_changed = &old[old_start_byte..old_end_byte];
        let rel_start = byte_start.saturating_sub(old_start_byte);
        let rel_end = byte_end.saturating_sub(old_start_byte);
        let mut new_changed = old_changed.to_string();
        new_changed.replace_range(rel_start..rel_end, &replace);
        let new_changed_lines = new_changed.lines().collect::<Vec<_>>();
        let old_count = hunk_end.saturating_sub(hunk_start);
        let new_count = old_start.saturating_sub(hunk_start)
            + new_changed_lines.len()
            + hunk_end.saturating_sub(old_end);
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk_start + 1,
            old_count,
            hunk_start + 1,
            new_count
        ));
        for line in &lines[hunk_start..old_start] {
            out.push_str(" ");
            out.push_str(line);
            out.push('\n');
        }
        for line in &lines[old_start..old_end] {
            out.push_str("-");
            out.push_str(line);
            out.push('\n');
        }
        for line in new_changed_lines {
            out.push_str("+");
            out.push_str(line);
            out.push('\n');
        }
        for line in &lines[old_end..hunk_end] {
            out.push_str(" ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn line_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (idx, ch) in text.char_indices() {
        if ch == '\n' && idx + 1 < text.len() {
            offsets.push(idx + 1);
        }
    }
    offsets
}

fn line_at_byte(offsets: &[usize], byte: usize) -> usize {
    match offsets.binary_search(&byte) {
        Ok(idx) => idx,
        Err(idx) => idx.saturating_sub(1),
    }
}

fn line_byte(offsets: &[usize], line: usize, text_len: usize) -> usize {
    offsets.get(line).copied().unwrap_or(text_len)
}

// ── V2: Hashedit (tag-based) ────────────────────────────────────────────

fn parse_tagged_line(raw: &str) -> Option<(usize, String)> {
    let stripped = raw.trim_start_matches([' ', '\t']);
    let num_tag = stripped
        .split_once(' ')
        .map_or(stripped, |(reference, _)| reference);
    let (num_str, tag) = num_tag.split_once('|')?;
    let line_num: usize = num_str.parse().ok()?;
    if tag.len() != 8 || !tag.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if line_num == 0 {
        return None;
    }
    Some((line_num, tag.to_ascii_lowercase()))
}

fn extract_line_info(lines_raw: &str) -> Result<Vec<(usize, String)>, ToolError> {
    let mut result = Vec::new();
    for line in lines_raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (line_num, tag) = parse_tagged_line(line).ok_or_else(|| {
            ToolError::Msg(format!(
                "Invalid tagged line format. Expected 'N|TAG content', got: '{}'",
                trimmed
            ))
        })?;
        result.push((line_num, tag));
    }
    if result.is_empty() {
        return Err(ToolError::Msg(
            "No valid tagged lines found. Copy lines from the read output exactly.".to_string(),
        ));
    }
    if result.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        || (result.len() > 2 && result.windows(2).any(|pair| pair[0].0 + 1 != pair[1].0))
    {
        return Err(ToolError::Msg(
            "Range references must ascend; lists longer than two must be contiguous.".into(),
        ));
    }
    Ok(result)
}

fn validate_tag(content_lines: &[&str], line_num: usize, tag: &str) -> Result<(), ToolError> {
    let idx = line_num.saturating_sub(1);
    let actual = content_lines.get(idx).ok_or_else(|| {
        ToolError::Msg(format!(
            "Line {} is out of range (file has {} lines)",
            line_num,
            content_lines.len()
        ))
    })?;
    let expected = crc32_hex(actual.as_bytes());
    if expected != tag {
        return Err(ToolError::Msg(format!(
            "Tag mismatch at line {}: expected {} but line content has tag {}. The file may have changed. Re-read and retry.",
            line_num, tag, expected
        )));
    }
    Ok(())
}

/// Use the boundary line's ending for inserted text. Leave all untouched bytes
/// unchanged, including mixed LF/CRLF files and a missing final newline.
fn line_ending(content: &str, offsets: &[usize], line: usize) -> &'static str {
    let start = offsets.get(line).copied().unwrap_or(content.len());
    let end = offsets.get(line + 1).copied().unwrap_or(content.len());
    let raw = &content[start..end];
    if raw.ends_with("\r\n") {
        "\r\n"
    } else if raw.ends_with('\n') {
        "\n"
    } else if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn replacement_text(text: &str, ending: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', ending)
}

fn validate_revision(path: &str, file_crc: &str, content: &str) -> Result<(), ToolError> {
    let actual = crc32_text(content);
    if !actual.eq_ignore_ascii_case(file_crc) {
        return Err(ToolError::Msg(format!(
            "File CRC mismatch for '{path}': expected {file_crc} but file now has {actual}. Re-read and retry."
        )));
    }
    Ok(())
}

fn handle_hashedit(
    content: &str,
    edits: &[EditOp],
) -> Result<Vec<(usize, usize, String)>, ToolError> {
    if edits.is_empty() {
        return Err(ToolError::Msg("Provide at least one edit.".into()));
    }
    let lines: Vec<&str> = content.lines().collect();
    let offsets = line_offsets(content);
    // Keep insertion provenance: replacing an empty line is also a zero-byte
    // range, but must not be mistaken for an insertion during validation.
    let mut ranges = Vec::new();
    for op in edits {
        if op.operation == EditOperation::Append {
            if op.line.is_some() || op.lines.is_some() || op.text.is_empty() {
                return Err(ToolError::Msg(
                    "Append requires non-empty text and no line references.".into(),
                ));
            }
            let ending = line_ending(content, &offsets, lines.len().saturating_sub(1));
            let prefix = if content.is_empty() || content.ends_with('\n') {
                ""
            } else {
                ending
            };
            let replacement = replacement_text(&op.text, ending);
            let suffix = if replacement.ends_with('\n') {
                ""
            } else {
                ending
            };
            ranges.push((
                content.len(),
                content.len(),
                format!("{prefix}{replacement}{suffix}"),
                true,
            ));
            continue;
        }
        let entries = match (&op.line, &op.lines) {
            (Some(line), None) => vec![parse_tagged_line(line).ok_or_else(|| {
                ToolError::Msg("invalid tagged line format. Expected N|TAG.".into())
            })?],
            (None, Some(lines)) => extract_line_info(lines)?,
            _ => {
                return Err(ToolError::Msg(
                    "Provide exactly one of line or lines.".into(),
                ));
            }
        };
        for (number, tag) in &entries {
            validate_tag(&lines, *number, tag)?;
        }
        let first = entries[0].0 - 1;
        let last = entries.last().unwrap().0 - 1;
        let start = offsets[first];
        let next = offsets.get(last + 1).copied().unwrap_or(content.len());
        let end = offsets[last] + lines[last].len();
        let ending = line_ending(content, &offsets, last);
        let replacement = replacement_text(&op.text, ending);
        match op.operation {
            EditOperation::Replace => ranges.push((start, end, replacement, false)),
            EditOperation::Delete => {
                if !op.text.is_empty() {
                    return Err(ToolError::Msg("Delete requires empty text.".into()));
                }
                ranges.push((start, next, String::new(), false));
            }
            EditOperation::InsertBefore | EditOperation::InsertAfter => {
                if entries.len() != 1 || op.text.is_empty() {
                    return Err(ToolError::Msg(
                        "Insertion requires one anchor and non-empty text.".into(),
                    ));
                }
                let suffix = if replacement.ends_with('\n') {
                    ""
                } else {
                    ending
                };
                if op.operation == EditOperation::InsertBefore {
                    ranges.push((start, start, format!("{replacement}{suffix}"), true));
                } else {
                    let prefix = if content[offsets[last]..next].ends_with('\n') {
                        ""
                    } else {
                        ending
                    };
                    ranges.push((next, next, format!("{prefix}{replacement}{suffix}"), true));
                }
            }
            EditOperation::Append => unreachable!(),
        }
    }
    // At a shared start, sort the insertion before the replacement. Reverse
    // application then replaces first and inserts before the replacement text.
    // This order does not depend on the order of operations in the request.
    ranges.sort_by_key(|(start, _, _, insertion)| (*start, !insertion));
    if ranges.windows(2).any(|pair| {
        let (left_start, left_end, _, left_insertion) = &pair[0];
        let (right_start, _, _, right_insertion) = &pair[1];
        let shared_start_conflict =
            left_start == right_start && !(*left_insertion && !*right_insertion);
        shared_start_conflict || left_end > right_start
    }) {
        return Err(ToolError::Msg(
            "Edit ranges overlap or multiple edits share an insertion point. Combine them into one edit.".into(),
        ));
    }
    Ok(ranges
        .into_iter()
        .map(|(start, end, text, _)| (start, end, text))
        .collect())
}

/// Small, agent-visible confirmation with fresh references. Large lines keep a
/// full-content tag, but clearly label their preview as incomplete.
fn edit_feedback(output: &str, ranges: &[(usize, usize, String)]) -> String {
    let offsets = line_offsets(output);
    let lines: Vec<_> = output.lines().collect();
    let mut feedback = String::new();
    let mut delta: isize = 0;
    let mut shown = std::collections::BTreeSet::new();
    for (start, end, text) in ranges {
        let new_start = start.saturating_add_signed(delta);
        let first = line_at_byte(&offsets, new_start).min(lines.len());
        let last = line_at_byte(
            &offsets,
            new_start.saturating_add(text.len().saturating_sub(1)),
        )
        .min(lines.len().saturating_sub(1));
        if feedback.len() < 3000 {
            feedback.push_str(&format!(
                "\nChanged result near lines {}-{}",
                first + 1,
                last.max(first) + 1
            ));
        }
        for index in first.saturating_sub(1)..=(last + 1).min(lines.len().saturating_sub(1)) {
            if feedback.len() >= 3000 || shown.len() >= 20 {
                break;
            }
            if let Some(line) = lines.get(index) {
                if shown.insert(index) {
                    let preview: String = line.chars().take(250).collect();
                    let suffix = if preview.len() < line.len() {
                        " [line preview truncated]"
                    } else {
                        ""
                    };
                    feedback.push_str(&format!(
                        "\n{}|{} {preview}{suffix}",
                        index + 1,
                        crc32_hex(line.as_bytes())
                    ));
                }
            }
        }
        delta += text.len() as isize - (end - start) as isize;
    }
    if shown.len() < lines.len() {
        feedback.push_str("\n[bounded confirmation excerpt; read for more context]");
    }
    feedback
}

async fn verify_source(
    path: &std::path::Path,
    target: &std::path::Path,
    expected: &[u8],
) -> Result<(), ToolError> {
    if tokio::fs::canonicalize(path).await? != target || tokio::fs::read(target).await? != expected
    {
        return Err(ToolError::Msg(
            "File changed before replacement. Re-read and retry.".into(),
        ));
    }
    Ok(())
}

// ── Tool implementation ──────────────────────────────────────────────────

impl EditTool {
    fn metadata(&self) -> (String, serde_json::Value) {
        let (desc, params) = (
                "Edit a file using tag-based line references. Use compact N|TAG references from read output. The file CRC rejects stale reads; this is not a concurrent-write transaction. Provide edits OR content for guarded whole-file replacement. References describe the original file. All operations are validated before atomic file replacement.".to_string(),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (relative or absolute)" },
                        "file_crc": { "type": "string", "pattern": "^[0-9a-fA-F]{8}$", "description": "8-char hex CRC-32 from read or the previous edit result [CRC: ...]" },
                        "content": { "type": "string", "description": "Guarded whole-file replacement. Requires file_crc; do not also supply edits. Content is written exactly." },
                        "edits": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "operation": { "type": "string", "enum": ["replace", "delete", "insert_before", "insert_after", "append"], "description": "Defaults to replace. Append needs no anchor; insert operations need one anchor." },
                                    "line": { "type": "string", "description": "Single-line reference from read: 'N|TAG'. Full tagged lines are also accepted." },
                                    "lines": { "type": "string", "description": "Inclusive range: two newline-separated N|TAG references, first and last. A contiguous list of full tagged lines is also accepted." },
                                    "text": { "type": "string", "description": "Replacement or inserted text. Replace with empty text clears content; operation delete removes complete lines." }
                                },
                                "required": ["text"],
                                "additionalProperties": false
                            },
                            "description": "Operations against the original file revision. One insertion may share a range boundary; insertions inside ranges and duplicate insertion points are rejected.",
                            "minItems": 1
                        }
                    },
                    "required": ["path", "file_crc"],
                    "additionalProperties": false
                }),
        );

        (desc, params)
    }
}

impl Tool for EditTool {
    const NAME: &'static str = "edit";

    type Error = ToolError;
    crate::agent::tools::expose_tool_errors!();
    type Args = EditArgs;
    type Output = String;

    fn description(&self) -> String {
        self.metadata().0
    }

    fn parameters(&self) -> serde_json::Value {
        self.metadata().1
    }

    async fn call(
        &self,
        _context: &mut rig::tool::ToolContext,
        args: EditArgs,
    ) -> Result<String, ToolError> {
        let path = crate::fs::expand_tilde(&args.path);
        let coaching = check_perm_path(&self.permission, &self.ask_tx, "edit", &path).await?;

        let (target, _lock) = crate::fs::lock_file_path(std::path::Path::new(&path)).await?;
        let bytes = tokio::fs::read(&target).await?;
        let content = String::from_utf8(bytes)
            .map_err(|_| ToolError::Msg("Cannot edit non-UTF-8 text.".into()))?;
        let file_crc = args.file_crc.as_ref().ok_or_else(|| {
            ToolError::Msg("Provide file_crc from read or the previous edit result.".into())
        })?;
        validate_revision(&path, file_crc, &content)?;
        let ranges = match (&args.edits, &args.content) {
            (Some(edits), None) => {
                let bytes = edits
                    .iter()
                    .try_fold(0usize, |n, op| n.checked_add(op.text.len()))
                    .ok_or_else(|| ToolError::Msg("Replacement text too large.".into()))?;
                if bytes as u64 > self.max_text_file_size {
                    return Err(ToolError::Msg(format!(
                        "Replacement text too large ({bytes} bytes); maximum is {} bytes.",
                        self.max_text_file_size
                    )));
                }
                handle_hashedit(&content, edits)?
            }
            (None, Some(replacement)) => {
                if replacement.len() as u64 > self.max_text_file_size {
                    return Err(ToolError::Msg(format!(
                        "Replacement too large; maximum is {} bytes.",
                        self.max_text_file_size
                    )));
                }
                vec![(0, content.len(), replacement.clone())]
            }
            _ => {
                return Err(ToolError::Msg(
                    "Provide exactly one of edits or content.".into(),
                ));
            }
        };
        let mut output = content.clone();
        for (start, end, replacement) in ranges.iter().rev() {
            output.replace_range(*start..*end, replacement);
        }
        let revision = crc32_text(&output);
        // Detect external changes observed before publication (or a no-op
        // result). External writers still need to cooperate to close the race.
        verify_source(std::path::Path::new(&path), &target, content.as_bytes()).await?;
        if output == content {
            let result = format!("No changes to {path}\n[CRC: {revision}]");
            return Ok(if let Some(msg) = coaching {
                format!("{msg}\n\n{result}")
            } else {
                result
            });
        }
        let diff = edit_ranges_diff(&path, &content, &ranges);
        crate::fs::atomic_write(&target, &output).await?;
        crate::agent::tools::untrack_read_path(&path);
        let mut result = format!(
            "Applied {} edit(s) to {}\n[CRC: {}]{}",
            ranges.len(),
            path,
            revision,
            edit_feedback(&output, &ranges)
        );
        set_last_edit_display_artifact(DisplayArtifact {
            kind: "patch".into(),
            label: path.clone().into(),
            contents: diff,
            extension: "patch".into(),
        });
        if let Some(msg) = coaching {
            result = format!("{}\n\n{}", msg, result);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn source_check_detects_raw_change_with_same_normalized_revision() {
        let path =
            std::env::temp_dir().join(format!("zerostack-source-check-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"original\r\n").unwrap();
        let target = std::fs::canonicalize(&path).unwrap();
        verify_source(&path, &target, b"original\r\n")
            .await
            .unwrap();
        std::fs::write(&path, b"original\n").unwrap();
        assert!(
            verify_source(&path, &target, b"original\r\n")
                .await
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn source_check_detects_retargeted_symlink() {
        let dir =
            std::env::temp_dir().join(format!("zerostack-source-link-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let first = dir.join("first");
        let second = dir.join("second");
        let link = dir.join("link");
        std::fs::write(&first, b"same").unwrap();
        std::fs::write(&second, b"same").unwrap();
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let target = std::fs::canonicalize(&link).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&second, &link).unwrap();
        assert!(verify_source(&link, &target, b"same").await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
