//! Bounded-memory text reads. Scan the complete file for a revision, but retain
//! only a continuous requested window. Normalize CRLF only for hashes.
use super::crc::Crc32;
use std::io::{self, BufRead};
#[cfg(test)]
use std::path::Path;

pub(super) const EXCERPT_BYTES: usize = 9_000;

pub(super) struct TextWindow {
    pub total_lines: usize,
    pub start: usize,
    pub end: usize,
    pub revision: String,
    pub excerpt: String,
    pub source: String,
    pub long_line: bool,
}

struct WindowBuilder {
    offset: usize,
    limit: usize,
    total: usize,
    count: usize,
    stopped: bool,
    excerpt: String,
    source: String,
    long_line: bool,
}

impl WindowBuilder {
    fn selected(&self) -> bool {
        self.total >= self.offset && self.count < self.limit && !self.stopped
    }

    fn line(&mut self, preview: &[u8], length: usize, crc: &Crc32, ending: &str) {
        if self.selected() {
            let prefix = format!("{:>3}|{} ", self.total + 1, crc.hex());
            let truncated = preview.len() < length;
            let suffix = if truncated {
                " [line preview truncated; tag covers the full line]"
            } else {
                ""
            };
            let required = prefix.len() + preview.len() + suffix.len() + 1;
            if self.count > 0 && self.excerpt.len() + required > EXCERPT_BYTES {
                self.stopped = true;
            } else {
                // A preview can end within a multibyte character. The complete
                // stream is validated separately; omit only the partial suffix.
                let valid = std::str::from_utf8(preview)
                    .map_or_else(|e| e.valid_up_to(), |_| preview.len());
                let preview = std::str::from_utf8(&preview[..valid]).expect("validated prefix");
                if self.count > 0 {
                    self.excerpt.push('\n');
                }
                self.excerpt.push_str(&prefix);
                self.excerpt.push_str(preview);
                self.excerpt.push_str(suffix);
                self.source.push_str(preview);
                self.source.push_str(ending);
                self.count += 1;
                self.long_line |= truncated;
                if truncated {
                    self.stopped = true;
                }
            }
        }
        self.total += 1;
    }
}

#[cfg(test)]
pub(super) fn read_window(path: &Path, offset: usize, limit: usize) -> io::Result<TextWindow> {
    read_window_file(std::fs::File::open(path)?, offset, limit)
}

pub(super) fn read_window_file(
    file: std::fs::File,
    offset: usize,
    limit: usize,
) -> io::Result<TextWindow> {
    let mut reader = std::io::BufReader::new(file);
    let mut builder = WindowBuilder {
        offset,
        limit,
        total: 0,
        count: 0,
        stopped: false,
        excerpt: String::new(),
        source: String::new(),
        long_line: false,
    };
    let mut file_crc = Crc32::new();
    let mut line_crc = Crc32::new();
    let mut preview = Vec::new();
    let mut length = 0usize;
    let mut pending_cr = false;
    let mut utf8 = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        utf8.extend_from_slice(buffer);
        match std::str::from_utf8(&utf8) {
            Ok(_) => utf8.clear(),
            Err(error) if error.error_len().is_none() => {
                utf8.drain(..error.valid_up_to());
            }
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "File is not UTF-8 text",
                ));
            }
        }
        for &byte in buffer {
            if pending_cr {
                if byte != b'\n' {
                    file_crc.update(b"\r");
                    line_crc.update(b"\r");
                    length += 1;
                    if builder.selected() && preview.len() < EXCERPT_BYTES - 128 {
                        preview.push(b'\r');
                    }
                }
                pending_cr = false;
                if byte == b'\n' {
                    file_crc.update(b"\n");
                    builder.line(&preview, length, &line_crc, "\r\n");
                    line_crc = Crc32::new();
                    preview.clear();
                    length = 0;
                    continue;
                }
            }
            match byte {
                b'\r' => pending_cr = true,
                b'\n' => {
                    file_crc.update(b"\n");
                    builder.line(&preview, length, &line_crc, "\n");
                    line_crc = Crc32::new();
                    preview.clear();
                    length = 0;
                }
                byte => {
                    file_crc.update(&[byte]);
                    line_crc.update(&[byte]);
                    length += 1;
                    if builder.selected() && preview.len() < EXCERPT_BYTES - 128 {
                        preview.push(byte);
                    }
                }
            }
        }
        let consumed = buffer.len();
        reader.consume(consumed);
    }
    if !utf8.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "File ends with incomplete UTF-8",
        ));
    }
    if pending_cr {
        file_crc.update(b"\r");
        line_crc.update(b"\r");
        length += 1;
        if builder.selected() && preview.len() < EXCERPT_BYTES - 128 {
            preview.push(b'\r');
        }
    }
    if length > 0 {
        builder.line(&preview, length, &line_crc, "");
    }
    let start = offset.min(builder.total);
    Ok(TextWindow {
        total_lines: builder.total,
        start,
        end: start + builder.count,
        revision: file_crc.hex(),
        excerpt: builder.excerpt,
        source: builder.source,
        long_line: builder.long_line,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::crc::crc32_hex;

    fn window(content: &[u8], offset: usize, limit: usize) -> io::Result<TextWindow> {
        let path = std::env::temp_dir().join(format!("zerostack-stream-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, content).unwrap();
        let result = read_window(&path, offset, limit);
        std::fs::remove_file(path).unwrap();
        result
    }

    #[test]
    fn streaming_hashes_and_lines_match_normalized_source() {
        for content in [
            "",
            "a",
            "a\n",
            "\n",
            "\r\n",
            "a\r\nb\nc\r\nd\r",
            "\r\r\n",
            "é🦀\n你好\r\n",
        ] {
            let result = window(content.as_bytes(), 0, 10).unwrap();
            assert_eq!(
                result.revision,
                crc32_hex(content.replace("\r\n", "\n").as_bytes()),
                "{content:?}"
            );
            assert_eq!(result.total_lines, content.lines().count(), "{content:?}");
            assert_eq!(result.source, content);
            for (tagged, source) in result.excerpt.lines().zip(content.lines()) {
                assert!(tagged.contains(&format!("|{} ", crc32_hex(source.as_bytes()))));
            }
        }
    }

    #[test]
    fn output_is_a_continuous_window_with_a_byte_budget() {
        let source = (1..=500)
            .map(|i| format!("line {i} {}\n", "x".repeat(100)))
            .collect::<String>();
        let result = window(source.as_bytes(), 4, 500).unwrap();
        assert_eq!(result.start, 4);
        assert!(result.end < result.total_lines);
        assert!(result.excerpt.len() <= EXCERPT_BYTES);
        assert_eq!(
            result.source,
            source
                .lines()
                .skip(4)
                .take(result.end - 4)
                .map(|s| format!("{s}\n"))
                .collect::<String>()
        );
        let next = window(source.as_bytes(), result.end, 2).unwrap();
        assert!(
            next.source
                .starts_with(&format!("line {} ", result.end + 1))
        );
    }

    #[test]
    fn very_long_unicode_line_has_full_hash_and_safe_preview() {
        let line = "🦀".repeat(100_000);
        let source = format!("{line}\r\nnext\n");
        let result = window(source.as_bytes(), 0, 10).unwrap();
        assert!(result.long_line);
        assert_eq!(result.end, 1);
        assert_eq!(result.total_lines, 2);
        assert!(result.excerpt.len() <= EXCERPT_BYTES);
        assert!(result.excerpt.contains(&crc32_hex(line.as_bytes())));
        assert!(!result.excerpt.contains('�'));
        assert_eq!(
            result.revision,
            crc32_hex(source.replace("\r\n", "\n").as_bytes())
        );
    }

    #[test]
    fn chunk_boundaries_preserve_unicode_and_crlf() {
        for prefix in [8190, 8191, 8192, 8193] {
            let source = format!("{}🦀\r\nlast", "a".repeat(prefix));
            let result = window(source.as_bytes(), 1, 1).unwrap();
            assert_eq!(result.source, "last");
            assert_eq!(
                result.revision,
                crc32_hex(source.replace("\r\n", "\n").as_bytes())
            );
            let source = format!("{}\r\nnext", "a".repeat(prefix));
            assert_eq!(
                window(source.as_bytes(), 1, 1).unwrap().revision,
                crc32_hex(source.replace("\r\n", "\n").as_bytes())
            );
        }
    }

    #[test]
    fn invalid_utf8_outside_requested_window_is_rejected() {
        for source in [&b"ok\n\xff"[..], &b"ok\n\xf0\x9f"[..]] {
            assert!(window(source, 0, 1).is_err());
        }
    }

    #[test]
    fn offset_past_eof_is_empty() {
        let result = window(b"one\ntwo\n", usize::MAX, usize::MAX).unwrap();
        assert_eq!((result.start, result.end), (2, 2));
        assert!(result.excerpt.is_empty());
    }
}
