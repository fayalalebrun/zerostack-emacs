#[cfg(feature = "multimodal")]
use base64::Engine;
#[cfg(feature = "multimodal")]
use base64::prelude::BASE64_STANDARD;
use rig::completion::ToolDefinition;
use rig::tool::Tool;

use crate::agent::tools::crc::crc32_hex;
use crate::agent::tools::{
    AskSender, ContextTracker, PermCheck, ReadArgs, ToolError, check_perm_path, edit_system,
};
use crate::config::types::EditSystem;

const DEFAULT_MAX_TEXT_SIZE: u64 = 1024 * 1024;

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

impl Tool for ReadTool {
    const NAME: &'static str = "read";

    type Error = ToolError;
    type Args = ReadArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
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

        ToolDefinition {
            name: "read".to_string(),
            description: desc,
            parameters: params,
        }
    }

    async fn call(&self, args: ReadArgs) -> Result<String, ToolError> {
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
                    return Ok(serde_json::json!({
                        "response": format!(
                            "Read image: {} ({}, {}x{}, {} bytes)",
                            path, image.mime, image.width, image.height, file_size
                        ),
                        "parts": [{
                            "type": "image",
                            "data": BASE64_STANDARD.encode(data),
                            "mimeType": image.mime,
                        }]
                    })
                    .to_string());
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

        Ok(info)
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
            .call(ReadArgs {
                path: path.to_string_lossy().to_string(),
                offset: None,
                limit: None,
            })
            .await
            .unwrap();
        let content = ToolResultContent::from_tool_output(output);

        assert!(matches!(content.first_ref(), ToolResultContent::Text(_)));
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
            .call(ReadArgs {
                path: path.to_string_lossy().to_string(),
                offset: None,
                limit: None,
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("UTF-8") || error.to_string().contains("utf-8"));
        std::fs::remove_file(path).unwrap();
    }
}
