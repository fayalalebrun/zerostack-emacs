#[cfg(feature = "multimodal")]
use base64::Engine;
#[cfg(feature = "multimodal")]
use base64::prelude::BASE64_STANDARD;
use rig::tool::Tool;

use crate::agent::tools::crc::crc32_hex;
use crate::agent::tools::{
    AskSender, ContextTracker, PermCheck, ReadArgs, ToolError, check_perm_path, edit_system,
};
use crate::config::types::EditSystem;

const DEFAULT_MAX_TEXT_SIZE: u64 = 1024 * 1024;

// Like read context metadata, snapshots follow the returned output into the runner.
static READ_DISPLAY_ARTIFACTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, crate::event::DisplayArtifact>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

pub(crate) fn take_read_display_artifact(output: &str) -> Option<crate::event::DisplayArtifact> {
    READ_DISPLAY_ARTIFACTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(output)
}

fn source_snapshot(
    path: &str,
    content: &str,
    start: usize,
    end: usize,
) -> crate::event::DisplayArtifact {
    // Keep the original line endings and the trailing newline, if present.
    let contents = content
        .split_inclusive('\n')
        .skip(start)
        .take(end - start)
        .collect();
    crate::event::DisplayArtifact {
        kind: "read-source".into(),
        label: std::path::Path::new(path)
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("source.txt"))
            .to_string_lossy()
            .as_ref()
            .into(),
        contents,
        extension: "".into(),
    }
}

pub struct ReadTool {
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
    pub max_text_file_size: u64,
    pub max_lines: u64,
    pub context_tracker: Option<ContextTracker>,
}

impl ReadTool {
    pub fn new(
        permission: Option<PermCheck>,
        ask_tx: Option<AskSender>,
        max_text_file_size: Option<u64>,
        max_lines: u64,
    ) -> Self {
        ReadTool {
            permission,
            ask_tx,
            max_text_file_size: max_text_file_size.unwrap_or(DEFAULT_MAX_TEXT_SIZE),
            max_lines,
            context_tracker: None,
        }
    }

    pub fn with_context_tracker(mut self, tracker: ContextTracker) -> Self {
        self.context_tracker = Some(tracker);
        self
    }
}

impl ReadTool {
    fn metadata(&self) -> (String, serde_json::Value) {
        let (desc, params) = match edit_system() {
            EditSystem::Similarity => (
                format!(
                    "Read a text file or view a PNG, JPEG, GIF, or WebP image. Text defaults to the first {} lines; use offset/limit for large text files.",
                    self.max_lines
                ),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (relative or absolute)" },
                        "offset": { "type": "integer", "description": "Line number to start from (1-indexed)" },
                        "limit": { "type": "integer", "description": "Maximum number of lines to read" }
                    },
                    "required": ["path"]
                }),
            ),
            EditSystem::Hashedit => (
                format!(
                    "Read text with CRC-32 tagged lines for tag-based editing, or view a PNG, JPEG, GIF, or WebP image. Each text line is prefixed with 'N|TAG' where TAG is an 8-char hex CRC-32. Defaults to first {} lines.",
                    self.max_lines
                ),
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to the file (relative or absolute)" },
                        "offset": { "type": "integer", "description": "Line number to start from (1-indexed)" },
                        "limit": { "type": "integer", "description": "Maximum number of lines to read" }
                    },
                    "required": ["path"]
                }),
            ),
        };

        (desc, params)
    }
}

impl Tool for ReadTool {
    const NAME: &'static str = "read";

    type Error = ToolError;
    crate::agent::tools::expose_tool_errors!();
    type Args = ReadArgs;
    type Output = rig::tool::ToolOutput;

    fn description(&self) -> String {
        self.metadata().0
    }

    fn parameters(&self) -> serde_json::Value {
        self.metadata().1
    }

    async fn call(
        &self,
        _context: &mut rig::tool::ToolContext,
        args: ReadArgs,
    ) -> Result<rig::tool::ToolOutput, ToolError> {
        let path = crate::fs::expand_tilde(&args.path);
        let coaching = check_perm_path(&self.permission, &self.ask_tx, "read", &path).await?;

        let offset = args.offset.unwrap_or(1).saturating_sub(1);
        let limit = args.limit.unwrap_or(self.max_lines as usize);

        if let Some(msg) = crate::agent::tools::track_read(&path, offset, limit) {
            return Err(ToolError::Msg(msg));
        }

        let metadata = tokio::fs::metadata(&path).await?;
        let file_size = metadata.len();
        #[cfg(feature = "multimodal")]
        if file_size <= crate::extras::multimodal::MAX_MEDIA_BYTES {
            let data = tokio::fs::read(&path).await?;
            match crate::extras::image_validate::validate(&data) {
                Ok(Some(image)) => {
                    let mut text = format!(
                        "Read image: {} ({}, {}x{}, {} bytes)",
                        path, image.mime, image.width, image.height, file_size
                    );
                    if let Some(coaching) = coaching {
                        text = format!("{coaching}\n\n{text}");
                    }
                    let rig::message::UserContent::Image(image) =
                        rig::message::UserContent::image_base64(
                            BASE64_STANDARD.encode(data),
                            Some(crate::agent::runner::image_media_type(&image.mime)),
                            None,
                        )
                    else {
                        unreachable!()
                    };
                    return Ok(rig::tool::ToolOutput::content(vec![
                        rig::message::ToolResultContent::Text(rig::message::Text::new(text)),
                        rig::message::ToolResultContent::Image(image),
                    ])
                    .map_err(|error| ToolError::Msg(error.to_string()))?);
                }
                Ok(None) => {}
                Err(error) => return Err(ToolError::Msg(error)),
            }
        } else if crate::extras::multimodal::detect_media(std::path::Path::new(&path))
            .is_some_and(|mime| mime.starts_with("image/"))
        {
            return Err(ToolError::Msg(format!(
                "Image too large ({} bytes). Maximum allowed image size is {} bytes.",
                file_size,
                crate::extras::multimodal::MAX_MEDIA_BYTES
            )));
        }
        if file_size > self.max_text_file_size {
            return Err(ToolError::Msg(format!(
                "File too large ({} bytes). Maximum allowed file size is {} bytes.",
                file_size, self.max_text_file_size
            )));
        }
        let content = tokio::fs::read_to_string(&path).await?;
        let total_lines = content.lines().count();

        let (start, end) = read_bounds(offset, limit, total_lines);

        let es = edit_system();

        let excerpt: String = match es {
            EditSystem::Hashedit => {
                // Annotate each line with CRC-32 tag
                content
                    .lines()
                    .skip(start)
                    .take(end - start)
                    .enumerate()
                    .map(|(i, line)| {
                        let line_num = start + i + 1;
                        let tag = crc32_hex(line.as_bytes());
                        let line_num_width = if total_lines >= 1000 { 4 } else { 3 };
                        format!(
                            "{:>width$}|{} {}",
                            line_num,
                            tag,
                            line,
                            width = line_num_width
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            EditSystem::Similarity => {
                // Plain text (original behavior)
                content
                    .lines()
                    .skip(start)
                    .take(end - start)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };

        let info = match es {
            EditSystem::Hashedit => {
                let file_crc = crc32_hex(content.replace("\r\n", "\n").as_bytes());
                format!(
                    "File: {} ({} lines total, lines {}-{}) [CRC: {}]\n\n{}",
                    path,
                    total_lines,
                    display_start(start, total_lines),
                    end,
                    file_crc,
                    excerpt
                )
            }
            EditSystem::Similarity => {
                format!(
                    "File: {} ({} lines total, showing lines {}-{})\n\n{}",
                    path,
                    total_lines,
                    display_start(start, total_lines),
                    end,
                    excerpt
                )
            }
        };

        let info = if end < total_lines {
            let remaining = total_lines - end;
            format!(
                "{}\n\n[truncated after {} lines — {} more lines (lines {}-{}); re-call with offset/limit to see more]",
                info,
                end - start,
                remaining,
                end + 1,
                total_lines,
            )
        } else {
            info
        };

        let loaded = crate::context::nested_agents_for_read(
            std::path::Path::new(&path),
            &crate::agent::tools::loaded_context_from(&self.context_tracker),
        );
        let loaded_paths: Vec<std::path::PathBuf> =
            loaded.iter().map(|(path, _)| path.clone()).collect();
        crate::agent::tools::mark_context_loaded_in(&self.context_tracker, &loaded_paths);

        let info = if loaded.is_empty() {
            info
        } else {
            format!(
                "{}\n\n{}",
                info,
                crate::agent::tools::format_context_reminder(&loaded).unwrap_or_default()
            )
        };

        let loaded_paths = loaded_paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        let info = match coaching {
            Some(msg) => format!("{}\n\n{}", msg, info),
            None => info,
        };
        let info = crate::agent::tools::truncate_live_tool_output(Self::NAME, &info);
        crate::agent::tools::register_read_context_metadata(&info, loaded_paths);
        READ_DISPLAY_ARTIFACTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(info.clone(), source_snapshot(&path, &content, start, end));

        Ok(rig::tool::ToolOutput::text(info))
    }
}

fn read_bounds(offset: usize, limit: usize, total_lines: usize) -> (usize, usize) {
    let start = offset.min(total_lines);
    let end = start.saturating_add(limit).min(total_lines);
    (start, end)
}

fn display_start(start: usize, total_lines: usize) -> usize {
    if total_lines == 0 { 0 } else { start + 1 }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "multimodal")]
    use rig::completion::message::ToolResultContent;
    #[cfg(feature = "multimodal")]
    use rig::tool::Tool;

    #[cfg(feature = "multimodal")]
    use super::ReadTool;
    use super::{display_start, read_bounds};
    #[cfg(feature = "multimodal")]
    use crate::agent::tools::ReadArgs;

    #[test]
    fn source_snapshot_preserves_basename_and_exact_excerpt() {
        for name in ["main.rs", "Makefile", ".gitignore"] {
            let snapshot = super::source_snapshot(
                &format!("/tmp/project/{name}"),
                "first\r\nsecond\r\nthird",
                1,
                2,
            );
            assert_eq!(snapshot.label, name);
            assert_eq!(snapshot.contents, "second\r\n");
            assert_eq!(super::source_snapshot(name, "a\nb", 1, 2).contents, "b");
            assert_eq!(super::source_snapshot(name, "a\nb", 2, 2).contents, "");
        }
    }

    #[tokio::test]
    async fn text_read_registers_source_snapshot_not_formatted_output() {
        use rig::tool::Tool;
        let path = std::env::temp_dir().join(format!("zerostack-read-{}.rs", uuid::Uuid::new_v4()));
        std::fs::write(&path, "// first\nfn main() {}\n// last\n").unwrap();
        let tool = super::ReadTool::new(None, None, None, 2000);
        let output = tool
            .call(
                &mut rig::tool::ToolContext::new(),
                crate::agent::tools::ReadArgs {
                    path: path.to_string_lossy().into_owned(),
                    offset: Some(2),
                    limit: Some(1),
                },
            )
            .await
            .unwrap()
            .into_content();
        let rig::completion::message::ToolResultContent::Text(text) = output.first().unwrap()
        else {
            panic!("expected text output");
        };
        assert!(text.text.contains("File:"));
        let snapshot = super::take_read_display_artifact(&text.text).unwrap();
        assert_eq!(snapshot.contents, "fn main() {}\n");
        assert_eq!(snapshot.label, path.file_name().unwrap().to_str().unwrap());
        assert!(super::take_read_display_artifact(&text.text).is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn read_bounds_clamps_offset_past_eof() {
        assert_eq!(read_bounds(20, 10, 5), (5, 5));
    }

    #[test]
    fn read_bounds_uses_requested_window_inside_file() {
        assert_eq!(read_bounds(2, 3, 10), (2, 5));
    }

    #[test]
    fn display_start_handles_empty_file() {
        assert_eq!(display_start(0, 0), 0);
    }

    #[cfg(feature = "multimodal")]
    #[tokio::test]
    async fn image_read_becomes_rig_image_tool_content() {
        let path =
            std::env::temp_dir().join(format!("zerostack-read-{}.png", uuid::Uuid::new_v4()));
        let bytes = crate::extras::image_validate::test_png();
        std::fs::write(&path, bytes).unwrap();
        let tool = ReadTool::new(None, None, None, 2000);

        let output = tool
            .call(
                &mut rig::tool::ToolContext::new(),
                ReadArgs {
                    path: path.to_string_lossy().to_string(),
                    offset: None,
                    limit: None,
                },
            )
            .await
            .unwrap();
        let content = output.into_content();

        assert!(matches!(
            content.first().unwrap(),
            ToolResultContent::Text(_)
        ));
        assert!(matches!(
            content.iter().nth(1),
            Some(ToolResultContent::Image(_))
        ));
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(feature = "multimodal")]
    #[tokio::test]
    async fn non_image_binary_still_returns_utf8_error() {
        let path =
            std::env::temp_dir().join(format!("zerostack-read-{}.bin", uuid::Uuid::new_v4()));
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
        let tool = ReadTool::new(None, None, None, 2000);

        let error = tool
            .call(
                &mut rig::tool::ToolContext::new(),
                ReadArgs {
                    path: path.to_string_lossy().to_string(),
                    offset: None,
                    limit: None,
                },
            )
            .await
            .unwrap_err();

        assert!(error.to_string().contains("UTF-8") || error.to_string().contains("utf-8"));
        std::fs::remove_file(path).unwrap();
    }
}
