use std::path::Path;

use rig::tool::Tool;

use crate::agent::tools::{AskSender, PermCheck, ToolError, WriteArgs, check_perm_path};

const DEFAULT_MAX_TEXT_SIZE: u64 = 1024 * 1024;

pub struct WriteTool {
    pub permission: Option<PermCheck>,
    pub ask_tx: Option<AskSender>,
    pub max_text_file_size: u64,
}

impl WriteTool {
    pub fn new(
        permission: Option<PermCheck>,
        ask_tx: Option<AskSender>,
        max_text_file_size: Option<u64>,
    ) -> Self {
        WriteTool {
            permission,
            ask_tx,
            max_text_file_size: max_text_file_size.unwrap_or(DEFAULT_MAX_TEXT_SIZE),
        }
    }
}

impl Tool for WriteTool {
    const NAME: &'static str = "write";

    type Error = ToolError;
    crate::agent::tools::expose_tool_errors!();
    type Args = WriteArgs;
    type Output = String;

    fn description(&self) -> String {
        "Create a new file with the given content. Fails if the file already exists — use edit for existing files. Automatically creates parent directories.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::tool::ToolContext,
        args: WriteArgs,
    ) -> Result<String, ToolError> {
        let expanded = crate::fs::expand_tilde(&args.path);
        let coaching = check_perm_path(&self.permission, &self.ask_tx, "write", &expanded).await?;

        let path = Path::new(&expanded);
        let bytes = args.content.len();
        if bytes as u64 > self.max_text_file_size {
            return Err(ToolError::Msg(format!(
                "File too large ({} bytes). Maximum allowed file size is {} bytes.",
                bytes, self.max_text_file_size
            )));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await?;
        }
        let (target, _lock) = crate::fs::lock_file_path(path).await?;
        // Creation uses the requested final entry, not a symlink's target.
        // This preserves create-only semantics for dangling and live symlinks.
        if tokio::fs::symlink_metadata(path).await.is_ok() {
            return Err(ToolError::Msg(format!(
                "File '{}' already exists. Use edit with file_crc for targeted changes or guarded full replacement.",
                expanded
            )));
        }
        crate::fs::atomic_create(&target, args.content.as_bytes())
            .await
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    ToolError::Msg(format!(
                        "File '{}' already exists. Use edit with file_crc.",
                        expanded
                    ))
                } else {
                    error.into()
                }
            })?;
        crate::agent::tools::untrack_read_path(&expanded);
        let mut result = format!(
            "Written {} bytes to {}\n[CRC: {}]",
            bytes,
            expanded,
            crate::agent::tools::crc::crc32_text(&args.content)
        );
        if let Some(msg) = coaching {
            result = format!("{}\n\n{}", msg, result);
        }
        Ok(result)
    }
}
