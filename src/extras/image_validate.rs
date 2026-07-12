use std::io::{BufReader, Cursor};

const MAX_DIMENSION: u32 = 16_384;
const MAX_PIXELS: u64 = 100_000_000;
const MAX_DECODE_BYTES: usize = 400 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

pub fn validate(data: &[u8]) -> Result<Option<ImageInfo>, String> {
    let info = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        validate_png(data)?
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        validate_jpeg(data)?
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        validate_gif(data)?
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        validate_webp(data)?
    } else {
        return Ok(None);
    };
    validate_dimensions(info)?;
    Ok(Some(info))
}

fn validate_dimensions(info: ImageInfo) -> Result<ImageInfo, String> {
    if info.width == 0 || info.height == 0 {
        return Err("image dimensions must be non-zero".to_string());
    }
    let pixels = u64::from(info.width) * u64::from(info.height);
    if info.width > MAX_DIMENSION || info.height > MAX_DIMENSION || pixels > MAX_PIXELS {
        return Err(format!(
            "image dimensions {}x{} exceed the limit",
            info.width, info.height
        ));
    }
    Ok(info)
}

fn validate_png(data: &[u8]) -> Result<ImageInfo, String> {
    if !data.ends_with(b"\0\0\0\0IEND\xaeB`\x82") {
        return Err("invalid PNG: missing IEND chunk".to_string());
    }
    let decoder = png::Decoder::new_with_limits(
        BufReader::new(Cursor::new(data)),
        png::Limits {
            bytes: MAX_DECODE_BYTES,
        },
    );
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("invalid PNG: {error}"))?;
    if reader.info().is_animated() {
        return Err("animated PNG is not supported".to_string());
    }
    let info = ImageInfo {
        mime: "image/png",
        width: reader.info().width,
        height: reader.info().height,
    };
    validate_dimensions(info)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "PNG output size exceeds platform limits".to_string())?;
    if size > MAX_DECODE_BYTES {
        return Err("decoded PNG exceeds memory limit".to_string());
    }
    let mut output = vec![0; size];
    reader
        .next_frame(&mut output)
        .map_err(|error| format!("invalid PNG: {error}"))?;
    Ok(info)
}

fn validate_jpeg(data: &[u8]) -> Result<ImageInfo, String> {
    if !data.ends_with(&[0xff, 0xd9]) {
        return Err("invalid JPEG: missing end marker".to_string());
    }
    let mut decoder = zune_jpeg::JpegDecoder::new(zune_core::bytestream::ZCursor::new(data));
    decoder
        .decode_headers()
        .map_err(|error| format!("invalid JPEG: {error}"))?;
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| "invalid JPEG: missing dimensions".to_string())?;
    let info = ImageInfo {
        mime: "image/jpeg",
        width: width.try_into().map_err(|_| "JPEG width is too large")?,
        height: height.try_into().map_err(|_| "JPEG height is too large")?,
    };
    validate_dimensions(info)?;
    let size = decoder
        .output_buffer_size()
        .ok_or_else(|| "JPEG output size exceeds platform limits".to_string())?;
    if size > MAX_DECODE_BYTES {
        return Err("decoded JPEG exceeds memory limit".to_string());
    }
    let mut output = vec![0; size];
    decoder
        .decode_into(&mut output)
        .map_err(|error| format!("invalid JPEG: {error}"))?;
    Ok(info)
}

fn validate_gif(data: &[u8]) -> Result<ImageInfo, String> {
    if !data.ends_with(&[0x3b]) {
        return Err("invalid GIF: missing trailer".to_string());
    }
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        std::num::NonZeroU64::new(MAX_DECODE_BYTES as u64).expect("limit is non-zero"),
    ));
    let mut decoder = options
        .read_info(Cursor::new(data))
        .map_err(|error| format!("invalid GIF: {error}"))?;
    let info = ImageInfo {
        mime: "image/gif",
        width: u32::from(decoder.width()),
        height: u32::from(decoder.height()),
    };
    validate_dimensions(info)?;
    if decoder
        .read_next_frame()
        .map_err(|error| format!("invalid GIF: {error}"))?
        .is_none()
    {
        return Err("invalid GIF: no image frame".to_string());
    }
    if decoder
        .read_next_frame()
        .map_err(|error| format!("invalid GIF: {error}"))?
        .is_some()
    {
        return Err("animated GIF is not supported".to_string());
    }
    Ok(info)
}

fn validate_webp(data: &[u8]) -> Result<ImageInfo, String> {
    let declared_size = data
        .get(4..8)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "invalid WebP: missing RIFF size".to_string())?;
    if u64::from(declared_size) + 8 != data.len() as u64 {
        return Err("invalid WebP: RIFF size does not match file length".to_string());
    }
    let mut decoder = image_webp::WebPDecoder::new(BufReader::new(Cursor::new(data)))
        .map_err(|error| format!("invalid WebP: {error}"))?;
    decoder.set_memory_limit(MAX_DECODE_BYTES);
    if decoder.is_animated() {
        return Err("animated WebP is not supported".to_string());
    }
    let (width, height) = decoder.dimensions();
    let info = ImageInfo {
        mime: "image/webp",
        width,
        height,
    };
    validate_dimensions(info)?;
    let size = decoder
        .output_buffer_size()
        .ok_or_else(|| "WebP output size exceeds platform limits".to_string())?;
    if size > MAX_DECODE_BYTES {
        return Err("decoded WebP exceeds memory limit".to_string());
    }
    let mut output = vec![0; size];
    decoder
        .read_image(&mut output)
        .map_err(|error| format!("invalid WebP: {error}"))?;
    Ok(info)
}

#[cfg(test)]
pub(crate) fn test_png() -> Vec<u8> {
    let mut data = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut data, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255, 0, 0, 255]).unwrap();
    }
    data
}

#[cfg(test)]
mod tests {
    use super::{test_png, validate};

    #[test]
    fn validates_png_and_rejects_truncation() {
        let data = test_png();
        let info = validate(&data).unwrap().unwrap();
        assert_eq!((info.mime, info.width, info.height), ("image/png", 1, 1));
        assert!(validate(&data[..data.len() - 4]).is_err());
    }

    #[test]
    fn accepts_static_gif_and_rejects_animation() {
        let static_gif = gif_bytes(1);
        let info = validate(&static_gif).unwrap().unwrap();
        assert_eq!((info.mime, info.width, info.height), ("image/gif", 1, 1));
        assert!(
            validate(&gif_bytes(2))
                .unwrap_err()
                .contains("animated GIF")
        );
    }

    #[test]
    fn rejects_truncated_jpeg_and_webp() {
        assert!(validate(b"\xff\xd8\xff\xe0").is_err());
        assert!(validate(b"RIFF\x04\x00\x00\x00WEBP").is_err());
    }

    fn gif_bytes(frames: usize) -> Vec<u8> {
        let mut data = Vec::new();
        {
            let mut encoder =
                gif::Encoder::new(&mut data, 1, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
            for _ in 0..frames {
                let frame = gif::Frame::from_indexed_pixels(1, 1, [0], None);
                encoder.write_frame(&frame).unwrap();
            }
        }
        data
    }
}
