#[cfg(feature = "multimodal")]
use base64::Engine;
#[cfg(feature = "multimodal")]
use base64::prelude::BASE64_STANDARD;
use rig::tool::Tool;

use crate::agent::tools::{
    AskSender, ContextTracker, PermCheck, ReadArgs, ToolError, check_perm_path,
};

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

fn source_artifact(path: &str, contents: String, truncated: bool) -> crate::event::DisplayArtifact {
    let name = std::path::Path::new(path)
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("source.txt"))
        .to_string_lossy();
    crate::event::DisplayArtifact {
        kind: "read-source".into(),
        label: if truncated {
            format!("{name} (truncated preview)").into()
        } else {
            name.as_ref().into()
        },
        contents,
        extension: "".into(),
    }
}

pub struct ReadTool {
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
    pub max_lines: u64,
    pub context_tracker: Option<ContextTracker>,
}

impl ReadTool {
    pub fn new(
        permission: Option<PermCheck>,
        ask_tx: Option<AskSender>,
        _max_text_file_size: Option<u64>,
        max_lines: u64,
    ) -> Self {
        ReadTool {
            permission,
            ask_tx,
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
        let (desc, params) = (
            format!(
                "Read text with CRC-32 tagged lines for tag-based editing, or view a PNG, JPEG, GIF, or WebP image. Each text line is prefixed with 'N|TAG' where TAG is an 8-char hex CRC-32. Defaults to at most {} lines, with a continuous byte-bounded excerpt. Large text files are scanned with bounded memory. A long line can return a labeled preview; its tag covers the full line.",
                self.max_lines
            ),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file (relative or absolute)" },
                    "offset": { "type": "integer", "minimum": 1, "description": "Line number to start from (1-indexed)" },
                    "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to read" }
                },
                "required": ["path"]
            }),
        );

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

        if args.offset == Some(0) || limit == 0 {
            return Err(ToolError::Msg("offset and limit must be positive.".into()));
        }
        #[allow(unused_mut)]
        let mut file = tokio::fs::File::open(&path).await?;
        #[cfg(feature = "multimodal")]
        {
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            // Probe by magic, not filename. Keep extensionless image support,
            // while avoiding a second full read of ordinary text files.
            let file_size = file.metadata().await?.len();
            let mut signature = Vec::new();
            (&mut file).take(12).read_to_end(&mut signature).await?;
            file.seek(std::io::SeekFrom::Start(0)).await?;
            let image_magic = signature.starts_with(b"\x89PNG\r\n\x1a\n")
                || signature.starts_with(&[0xff, 0xd8, 0xff])
                || signature.starts_with(b"GIF87a")
                || signature.starts_with(b"GIF89a")
                || (signature.len() >= 12
                    && &signature[..4] == b"RIFF"
                    && &signature[8..12] == b"WEBP");
            let image_path = crate::extras::multimodal::detect_media(std::path::Path::new(&path))
                .is_some_and(|mime| mime.starts_with("image/"));
            if (image_magic || image_path) && file_size > crate::extras::multimodal::MAX_MEDIA_BYTES
            {
                return Err(ToolError::Msg(format!(
                    "Image too large ({file_size} bytes). Maximum is {} bytes.",
                    crate::extras::multimodal::MAX_MEDIA_BYTES
                )));
            }
            if image_magic {
                let mut data = Vec::new();
                (&mut file)
                    .take(crate::extras::multimodal::MAX_MEDIA_BYTES + 1)
                    .read_to_end(&mut data)
                    .await?;
                if data.len() as u64 > crate::extras::multimodal::MAX_MEDIA_BYTES {
                    return Err(ToolError::Msg("Image exceeds the size limit.".into()));
                }
                match crate::extras::image_validate::validate(&data) {
                    Ok(Some(image)) => {
                        let mut text = format!(
                            "Read image: {} ({}, {}x{}, {} bytes)",
                            path,
                            image.mime,
                            image.width,
                            image.height,
                            data.len()
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
                    Ok(None) => {
                        file.seek(std::io::SeekFrom::Start(0)).await?;
                    }
                    Err(error) => return Err(ToolError::Msg(error)),
                }
            }
        }
        let canonical = tokio::fs::canonicalize(&path).await?;
        let file = file.into_std().await;
        let window = tokio::task::spawn_blocking(move || {
            super::read_text::read_window_file(file, offset, limit)
        })
        .await
        .map_err(|error| ToolError::Msg(error.to_string()))??;
        let start = window.start;
        let end = window.end;
        let mut info = format!(
            "File: {} ({} lines total, lines {}-{}) [CRC: {}]\n\n{}",
            path,
            window.total_lines,
            display_start(start, window.total_lines),
            end,
            window.revision,
            window.excerpt
        );
        if end < window.total_lines {
            info.push_str(&format!(
                "\n\n[excerpt ends at line {end}; continue with offset={} and limit]",
                end + 1
            ));
        }
        if window.long_line {
            info.push_str(
                "\n[long line preview is incomplete; do not treat it as the full source line]",
            );
        }
        if let Some(warning) = crate::agent::tools::track_read(
            &canonical.to_string_lossy(),
            &window.revision,
            start,
            end - start,
        ) {
            info.push_str(&format!("\n{warning}"));
        }
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
        crate::agent::tools::register_read_context_metadata(&info, loaded_paths);
        READ_DISPLAY_ARTIFACTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                info.clone(),
                source_artifact(&path, window.source, window.long_line),
            );

        Ok(rig::tool::ToolOutput::text(info))
    }
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
    use super::display_start;
    #[cfg(feature = "multimodal")]
    use crate::agent::tools::ReadArgs;

    #[test]
    fn source_artifact_preserves_basename_and_exact_excerpt() {
        for name in ["main.rs", "Makefile", ".gitignore"] {
            let artifact =
                super::source_artifact(&format!("/tmp/project/{name}"), "second\r\n".into(), false);
            assert_eq!(artifact.label, name);
            assert_eq!(artifact.contents, "second\r\n");
        }
        assert!(
            super::source_artifact("file", "preview".into(), true)
                .label
                .contains("truncated")
        );
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
