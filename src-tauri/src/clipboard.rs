use arboard::Clipboard;
use sha2::{Digest, Sha256};

pub enum ClipboardContent {
    Text { body: String, hash: String },
    Image(CapturedImage),
}

impl ClipboardContent {
    pub fn hash(&self) -> &str {
        match self {
            Self::Text { hash, .. } => hash,
            Self::Image(image) => &image.hash,
        }
    }
}

pub struct CapturedImage {
    pub png: Vec<u8>,
    pub thumbnail_png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub hash: String,
}

pub fn read_clipboard() -> Result<ClipboardContent, String> {
    let mut clipboard = Clipboard::new().map_err(error)?;
    if let Ok(image) = clipboard.get_image() {
        return encode_image(image.width, image.height, image.bytes.as_ref())
            .map(ClipboardContent::Image);
    }
    let body = clipboard.get_text().map_err(error)?;
    let hash = content_hash(b"text", body.as_bytes());
    Ok(ClipboardContent::Text { body, hash })
}

pub fn read_clipboard_image() -> Result<CapturedImage, String> {
    let mut clipboard = Clipboard::new().map_err(error)?;
    let image = clipboard.get_image().map_err(error)?;
    encode_image(image.width, image.height, image.bytes.as_ref())
}

fn encode_image(width: usize, height: usize, rgba: &[u8]) -> Result<CapturedImage, String> {
    let expected = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "clipboard image is too large".to_string())?;
    if rgba.len() != expected {
        return Err("clipboard image has invalid pixel data".to_string());
    }
    let width = u32::try_from(width).map_err(|_| "clipboard image is too wide".to_string())?;
    let height = u32::try_from(height).map_err(|_| "clipboard image is too tall".to_string())?;
    let png = encode_rgba_png(width, height, rgba)?;
    let (thumbnail_width, thumbnail_height) = bounded_dimensions(width, height, 1_200, 900);
    let thumbnail_rgba = if (thumbnail_width, thumbnail_height) == (width, height) {
        rgba.to_vec()
    } else {
        resize_nearest(rgba, width, height, thumbnail_width, thumbnail_height)
    };
    let thumbnail_png = encode_rgba_png(thumbnail_width, thumbnail_height, &thumbnail_rgba)?;
    let mut dimensions = [0_u8; 8];
    dimensions[..4].copy_from_slice(&width.to_le_bytes());
    dimensions[4..].copy_from_slice(&height.to_le_bytes());
    let hash = content_hash(&dimensions, rgba);
    Ok(CapturedImage {
        png,
        thumbnail_png,
        width,
        height,
        hash,
    })
}

fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(error)?;
        writer.write_image_data(rgba).map_err(error)?;
    }
    Ok(output)
}

fn bounded_dimensions(
    width: u32,
    height: u32,
    maximum_width: u32,
    maximum_height: u32,
) -> (u32, u32) {
    if width <= maximum_width && height <= maximum_height {
        return (width, height);
    }
    let scale = (maximum_width as f64 / width as f64).min(maximum_height as f64 / height as f64);
    (
        (width as f64 * scale).round().max(1.0) as u32,
        (height as f64 * scale).round().max(1.0) as u32,
    )
}

fn resize_nearest(
    source: &[u8],
    width: u32,
    height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    let mut target = vec![0; target_width as usize * target_height as usize * 4];
    for target_y in 0..target_height {
        let source_y = target_y as usize * height as usize / target_height as usize;
        for target_x in 0..target_width {
            let source_x = target_x as usize * width as usize / target_width as usize;
            let source_index = (source_y * width as usize + source_x) * 4;
            let target_index = (target_y as usize * target_width as usize + target_x as usize) * 4;
            target[target_index..target_index + 4]
                .copy_from_slice(&source[source_index..source_index + 4]);
        }
    }
    target
}

#[cfg(target_os = "macos")]
pub fn pasteboard_change_count() -> i64 {
    use objc2_app_kit::NSPasteboard;
    NSPasteboard::generalPasteboard().changeCount() as i64
}

#[cfg(not(target_os = "macos"))]
pub fn pasteboard_change_count() -> i64 {
    0
}

pub fn content_hash(prefix: &[u8], content: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(prefix);
    digest.update(content);
    format!("{:x}", digest.finalize())
}

#[cfg(target_os = "macos")]
pub fn frontmost_bundle_identifier() -> Option<String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSWorkspace;

    let _main_thread = MainThreadMarker::new()?;
    let workspace = NSWorkspace::sharedWorkspace();
    let application = workspace.frontmostApplication()?;
    application
        .bundleIdentifier()
        .map(|value| value.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_bundle_identifier() -> Option<String> {
    None
}

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::{content_hash, encode_image};

    #[test]
    fn image_encoding_is_png_and_stably_hashed() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
        let first = encode_image(2, 1, &rgba).unwrap();
        let second = encode_image(2, 1, &rgba).unwrap();
        assert_eq!(&first.png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&first.thumbnail_png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(first.hash, second.hash);
        assert_eq!((first.width, first.height), (2, 1));
    }

    #[test]
    fn hash_distinguishes_content_kinds() {
        assert_ne!(
            content_hash(b"text", b"same"),
            content_hash(b"image", b"same")
        );
    }
}
