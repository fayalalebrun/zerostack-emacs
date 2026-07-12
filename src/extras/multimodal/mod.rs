use std::path::{Path, PathBuf};

use compact_str::CompactString;
use uuid::Uuid;

use crate::session::SessionAttachment;

/// Maximum file size for media attachments: 20 MB.
pub const MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;

/// Represents a media file attached to a user message.
/// The raw bytes are held in memory and converted to rig message content
/// when the message is submitted.
#[derive(Debug, Clone)]
pub enum MediaAttachment {
    Image {
        path: PathBuf,
        data: Vec<u8>,
        mime: String,
    },
    Audio {
        path: PathBuf,
        data: Vec<u8>,
        mime: String,
    },
    Document {
        path: PathBuf,
        data: Vec<u8>,
        mime: String,
    },
}

impl MediaAttachment {
    pub fn size(&self) -> usize {
        match self {
            MediaAttachment::Image { data, .. }
            | MediaAttachment::Audio { data, .. }
            | MediaAttachment::Document { data, .. } => data.len(),
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            MediaAttachment::Image { path, .. }
            | MediaAttachment::Audio { path, .. }
            | MediaAttachment::Document { path, .. } => path,
        }
    }

    fn data(&self) -> &[u8] {
        match self {
            MediaAttachment::Image { data, .. }
            | MediaAttachment::Audio { data, .. }
            | MediaAttachment::Document { data, .. } => data,
        }
    }

    fn mime(&self) -> &str {
        match self {
            MediaAttachment::Image { mime, .. }
            | MediaAttachment::Audio { mime, .. }
            | MediaAttachment::Document { mime, .. } => mime,
        }
    }
}

pub fn persist_attachment(
    session_id: &str,
    attachment: &MediaAttachment,
) -> std::io::Result<SessionAttachment> {
    let dir = crate::session::storage::media_dir(session_id);
    std::fs::create_dir_all(&dir)?;
    let extension = attachment
        .path()
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("media");
    let stored_name = format!("{}.{}", Uuid::new_v4(), extension);
    std::fs::write(dir.join(&stored_name), attachment.data())?;
    Ok(SessionAttachment {
        filename: CompactString::new(
            attachment
                .path()
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("attachment"),
        ),
        stored_name: CompactString::new(stored_name),
        mime: CompactString::new(attachment.mime()),
        size_bytes: attachment.size() as u64,
    })
}

pub fn load_persisted_attachment(
    session_id: &str,
    attachment: &SessionAttachment,
) -> std::io::Result<MediaAttachment> {
    let stored_name = Path::new(attachment.stored_name.as_str());
    if stored_name.file_name() != Some(stored_name.as_os_str()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid stored attachment name",
        ));
    }
    let path = crate::session::storage::media_dir(session_id).join(stored_name);
    let data = std::fs::read(&path)?;
    if data.len() as u64 > MAX_MEDIA_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stored attachment exceeds size limit",
        ));
    }
    let mime = attachment.mime.to_string();
    Ok(if mime.starts_with("image/") {
        MediaAttachment::Image { path, data, mime }
    } else if mime.starts_with("audio/") {
        MediaAttachment::Audio { path, data, mime }
    } else {
        MediaAttachment::Document { path, data, mime }
    })
}

/// Check whether a file extension indicates multi-modal media (not text).
/// Returns the MIME type string if recognized, `None` otherwise.
pub fn detect_media(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "mp3" => Some("audio/mpeg"),
        "wav" => Some("audio/wav"),
        "ogg" => Some("audio/ogg"),
        "flac" => Some("audio/flac"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
}

/// Load a media file from disk. The caller must have already verified the
/// path exists and is a file. Returns an error if the file is too large.
pub fn load_attachment(path: &Path) -> std::io::Result<MediaAttachment> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_MEDIA_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "file too large: {} (max {} bytes)",
                path.display(),
                MAX_MEDIA_BYTES
            ),
        ));
    }

    let data = std::fs::read(path)?;
    let mime = detect_media(path)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unknown media type: {}", path.display()),
            )
        })?
        .to_string();

    // We already know the mime from detect_media — dispatch on the prefix.
    let path = path.to_path_buf();
    Ok(if mime.starts_with("image/") {
        MediaAttachment::Image { path, data, mime }
    } else if mime.starts_with("audio/") {
        MediaAttachment::Audio { path, data, mime }
    } else {
        MediaAttachment::Document { path, data, mime }
    })
}
