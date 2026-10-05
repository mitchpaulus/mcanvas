//! Embedded image data: pixels from the clipboard are stored in the canvas
//! file as base64 PNG (the `data` field of an image node).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

/// Encode RGBA8 pixels as a base64 PNG string.
pub fn rgba_to_png_base64(width: u32, height: u32, rgba: Vec<u8>) -> Result<String, String> {
    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "pixel buffer does not match image size".to_string())?;
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(B64.encode(png))
}

/// Decode a base64 image (PNG) into RGBA8 pixels.
pub fn base64_to_rgba(data: &str) -> Result<image::RgbaImage, String> {
    // Tolerate line breaks from hand-edited or wrapped files.
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = B64.decode(compact).map_err(|e| format!("bad base64: {e}"))?;
    Ok(image::load_from_memory(&bytes).map_err(|e| e.to_string())?.to_rgba8())
}

/// Decode a base64 image (PNG) into a Slint image.
pub fn slint_image_from_base64(data: &str) -> Result<slint::Image, String> {
    let img = base64_to_rgba(data)?;
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        img.as_raw(),
        img.width(),
        img.height(),
    );
    Ok(slint::Image::from_rgba8(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip() {
        // 2x1: opaque red, half-transparent blue.
        let px = vec![255, 0, 0, 255, 0, 0, 255, 128];
        let b64 = rgba_to_png_base64(2, 1, px.clone()).unwrap();
        assert_eq!(base64_to_rgba(&b64).unwrap().into_raw(), px);
        let img = slint_image_from_base64(&b64).unwrap();
        assert_eq!((img.size().width, img.size().height), (2, 1));
        let buf = img.to_rgba8().unwrap();
        let back: Vec<u8> = buf.as_bytes().to_vec();
        assert_eq!(back, vec![255, 0, 0, 255, 0, 0, 255, 128]);
        assert!(rgba_to_png_base64(3, 3, vec![0; 4]).is_err());
        assert!(slint_image_from_base64("not base64!").is_err());
    }
}
