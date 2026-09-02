//! Decoding the image files a preset references (decision 59).
//!
//! A preset's texture pack (`sampler_fw_clouds` -> `textures/clouds.jpg`) and
//! its sprites (`SpriteName=sprites\x1.png`) both name image files that live
//! beside the preset. `player-render` does no IO -- it is handed bytes -- so
//! this is where bytes become RGBA, the one step between "a file the host read"
//! and "a texture a shader or sprite pass samples".
//!
//! PNG and JPEG only. That is deliberate rather than a limitation left to be
//! discovered: the corpus ships `jpg`, `png` and `jfif` (which is JPEG), and
//! nothing else -- so this is the whole format, and no other decoder has to
//! compile to wasm.

/// A decoded image, RGBA8, ready to upload to a GPU texture.
#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decodes PNG/JPEG bytes into RGBA8.
///
/// `None` means the bytes are not an image this build can read -- a missing
/// file is the host's to report, and a corrupt one is not an error to panic
/// over. The caller decides whether that is a placeholder or a report.
pub fn decode(bytes: &[u8]) -> Option<DecodedImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(DecodedImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal 2x2 PNG, opaque, four distinct pixels -- enough to prove the
    /// decode round-trips width, height and colour order.
    fn png_2x2() -> Vec<u8> {
        // A hand-built PNG is brittle; generate one through the `image` crate's
        // own encoder, then decode it back. That is still a real byte-level
        // round trip (encode -> bytes -> decode), not a self-check.
        let mut buf = image::RgbaImage::new(2, 2);
        buf.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        buf.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        buf.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        buf.put_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn decodes_png_to_rgba_in_row_major_order() {
        let bytes = png_2x2();
        let d = decode(&bytes).expect("decodes");
        assert_eq!((d.width, d.height), (2, 2));
        assert_eq!(&d.rgba[0..4], &[255, 0, 0, 255], "top-left is red");
        assert_eq!(&d.rgba[4..8], &[0, 255, 0, 255], "top-right is green");
        assert_eq!(&d.rgba[8..12], &[0, 0, 255, 255], "bottom-left is blue");
    }

    #[test]
    fn garbage_is_none_rather_than_a_panic() {
        assert!(decode(b"not an image").is_none());
        assert!(decode(&[]).is_none());
    }
}
