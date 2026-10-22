//! Independent conversation snapshots. Forks never create a Git worktree.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, ensure};
use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{MessageRole, Session, SessionAttachment, storage};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForkSource {
    pub session_id: CompactString,
    /// Exclusive boundary in the source's currently retained message list.
    pub message_index: usize,
}

impl Session {
    /// Copy the retained conversation, optionally stopping before a user message.
    pub fn fork(&self, before: Option<usize>) -> anyhow::Result<Self> {
        let end = before.unwrap_or(self.messages.len());
        if let Some(index) = before {
            ensure!(
                self.messages
                    .get(index)
                    .is_some_and(|message| message.role == MessageRole::User),
                "fork index must identify a retained user message"
            );
        }
        let mut fork = self.clone();
        fork.id = Uuid::new_v4().to_string().into();
        fork.truncate_to(end);
        fork.name = format!("{} (fork)", fork.title()).into();
        fork.fork_source = Some(ForkSource {
            session_id: self.id.clone(),
            message_index: end,
        });
        fork.created_at = chrono::Utc::now().to_rfc3339().into();
        fork.updated_at = fork.created_at.clone();
        fork.rewind_undo = None;
        fork.parent_session_id = None;
        fork.subagent_access = None;
        fork.permission_allowlist.clear();
        fork.goal = None;
        fork.provider_calls.clear();
        fork.total_input_tokens = 0;
        fork.total_cached_input_tokens = 0;
        fork.total_output_tokens = 0;
        fork.total_reasoning_tokens = 0;
        fork.total_cost = 0.0;
        fork.total_estimated_tokens = fork
            .messages
            .iter()
            .map(|message| message.estimated_tokens)
            .sum();
        fork.reset_calibration();
        #[cfg(feature = "multimodal")]
        fork.pending_media.clear();
        // Compaction metadata is meaningful only while its summary remains.
        if fork.compacted_context().0.is_none() {
            fork.compactions.clear();
        }
        Ok(fork)
    }
}

/// Save a complete, independent fork. Publish its JSON only after all assets exist.
/// On error, remove only the files/directories created by this operation.
pub fn save(source: &Session, before: Option<usize>) -> anyhow::Result<Session> {
    let mut fork = source.fork(before)?;
    let media_source = storage::media_dir(&source.id);
    let outputs_source = storage::tool_output_dir(&source.id);
    let media_target = storage::media_dir(&fork.id);
    let outputs_target = storage::tool_output_dir(&fork.id);
    let session_path = storage::session_path(&fork.id);
    let temporary = session_path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let mut created_dirs = Vec::new();
    let mut copied = HashMap::<PathBuf, PathBuf>::new();
    let mut temporary_created = false;
    let mut published = false;
    let result = (|| -> anyhow::Result<()> {
        // The excluded user message becomes an editable draft in the client.
        // Give its attachments independent storage too.
        if let Some(index) = before {
            for attachment in &source.messages[index].attachments {
                copy_attachment(
                    attachment,
                    &media_source,
                    &media_target,
                    &mut created_dirs,
                    &mut copied,
                )?;
            }
        }
        for message in &mut fork.messages {
            for attachment in &message.attachments {
                copy_attachment(
                    attachment,
                    &media_source,
                    &media_target,
                    &mut created_dirs,
                    &mut copied,
                )?;
            }
            if let Some(result) = &mut message.tool_result {
                for attachment in &result.attachments {
                    copy_attachment(
                        attachment,
                        &media_source,
                        &media_target,
                        &mut created_dirs,
                        &mut copied,
                    )?;
                }
                for path in [&mut result.output_path, &mut result.display_path] {
                    if let Some(original) = path.as_ref()
                        && let Some(target) = copy_output(
                            Path::new(original.as_str()),
                            &outputs_source,
                            &outputs_target,
                            &mut created_dirs,
                            &mut copied,
                        )?
                    {
                        *path = Some(target.to_string_lossy().into_owned().into());
                    }
                }
            }
            // Older sessions stored the output path only in the result text.
            if message.role == MessageRole::ToolResult {
                let mut rest = message.content.as_str();
                while let Some((_, tail)) = rest.split_once("[full output saved to: ") {
                    let Some((path, remainder)) = tail.split_once(';') else {
                        break;
                    };
                    copy_output(
                        Path::new(path),
                        &outputs_source,
                        &outputs_target,
                        &mut created_dirs,
                        &mut copied,
                    )?;
                    rest = remainder;
                }
            }
        }
        let mut replacements: Vec<_> = copied
            .iter()
            .filter(|(source, _)| source.starts_with(&outputs_source))
            .collect();
        // Replace longer paths first, so one filename cannot replace another's prefix.
        replacements.sort_by_key(|(source, _)| std::cmp::Reverse(source.as_os_str().len()));
        for message in &mut fork.messages {
            let old_estimate = Session::estimate_tokens(&message.content);
            let mut content = message.content.to_string();
            for (source, target) in &replacements {
                content = content.replace(
                    source.to_string_lossy().as_ref(),
                    target.to_string_lossy().as_ref(),
                );
            }
            message.estimated_tokens = message
                .estimated_tokens
                .saturating_sub(old_estimate)
                .saturating_add(Session::estimate_tokens(&content));
            message.content = content.into();
        }
        fork.total_estimated_tokens = fork
            .messages
            .iter()
            .map(|message| message.estimated_tokens)
            .sum();
        std::fs::create_dir_all(storage::session_dir())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        temporary_created = true;
        // Unlike save_session(), do not substitute the source process's active goal.
        file.write_all(&serde_json::to_vec(&fork)?)?;
        file.sync_all()?;
        std::fs::hard_link(&temporary, &session_path)?;
        published = true;
        std::fs::File::open(storage::session_dir())?.sync_all()?;
        Ok(())
    })();
    if temporary_created {
        let _ = std::fs::remove_file(&temporary);
    }
    if let Err(error) = result {
        if published {
            let _ = std::fs::remove_file(&session_path);
        }
        for directory in created_dirs.iter().rev() {
            let _ = std::fs::remove_dir_all(directory);
        }
        return Err(error.context("could not save fork; original session is unchanged"));
    }
    Ok(fork)
}

fn copy_attachment(
    attachment: &SessionAttachment,
    source: &Path,
    target: &Path,
    created: &mut Vec<PathBuf>,
    copied: &mut HashMap<PathBuf, PathBuf>,
) -> anyhow::Result<()> {
    let name = Path::new(attachment.stored_name.as_str());
    ensure!(
        name.components().count() == 1
            && matches!(name.components().next(), Some(Component::Normal(_))),
        "invalid stored attachment name"
    );
    copy_file(&source.join(name), source, target, created, copied)?;
    Ok(())
}

fn copy_output(
    path: &Path,
    source: &Path,
    target: &Path,
    created: &mut Vec<PathBuf>,
    copied: &mut HashMap<PathBuf, PathBuf>,
) -> anyhow::Result<Option<PathBuf>> {
    if !path.starts_with(source) {
        // A project-local source path remains valid in the same working directory.
        return Ok(None);
    }
    copy_file(path, source, target, created, copied).map(Some)
}

fn copy_file(
    path: &Path,
    source: &Path,
    target: &Path,
    created: &mut Vec<PathBuf>,
    copied: &mut HashMap<PathBuf, PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(target) = copied.get(path) {
        return Ok(target.clone());
    }
    let relative = path.strip_prefix(source)?;
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "invalid session asset path: {}",
        path.display()
    );
    let canonical = path
        .canonicalize()
        .with_context(|| format!("missing fork asset: {}", path.display()))?;
    ensure!(
        canonical.starts_with(source.canonicalize()?) && canonical.is_file(),
        "session asset is outside its storage directory: {}",
        path.display()
    );
    if !created.iter().any(|directory| directory == target) {
        std::fs::create_dir_all(target.parent().context("asset directory has no parent")?)?;
        // Never overwrite or later clean up another session's directory.
        std::fs::create_dir(target)?;
        created.push(target.to_path_buf());
    }
    let destination = target.join(relative);
    std::fs::create_dir_all(destination.parent().context("asset has no parent")?)?;
    std::fs::copy(path, &destination)?;
    copied.insert(path.to_path_buf(), destination.clone());
    Ok(destination)
}

#[cfg(test)]
mod tests;
