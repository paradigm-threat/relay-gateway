use anyhow::Context;
use image::ImageFormat;
use std::io::Cursor;

/// Decode raster bytes, resize so width is at most `max_w`, re-encode as WebP.
pub fn resize_to_webp(raw: &[u8], max_w: u32) -> anyhow::Result<Vec<u8>> {
    let img = image::load_from_memory(raw).context("decode image")?;
    let (w, h) = (img.width(), img.height());
    let img = if max_w > 0 && w > max_w {
        let nh = ((h as f64) * (max_w as f64 / w as f64)).round() as u32;
        img.resize_exact(max_w, nh.max(1), image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::WebP)
        .context("encode webp")?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};

    fn sample_wide_png() -> Vec<u8> {
        let img = RgbaImage::from_pixel(400, 2, Rgba([10u8, 40u8, 200u8, 255u8]));
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        let mut c = Cursor::new(Vec::new());
        dyn_img.write_to(&mut c, ImageFormat::Png).unwrap();
        c.into_inner()
    }

    #[test]
    fn resize_to_webp_shrinks_width() {
        let png = sample_wide_png();
        let webp = resize_to_webp(&png, 50).expect("webp");
        assert!(webp.len() > 16);
        assert_eq!(&webp[0..4], b"RIFF");
        let decoded = image::load_from_memory(&webp).expect("decode webp");
        assert_eq!(decoded.width(), 50);
    }

    #[test]
    fn resize_to_webp_no_op_when_under_cap() {
        let png = sample_wide_png();
        let webp = resize_to_webp(&png, 800).expect("webp");
        let decoded = image::load_from_memory(&webp).expect("decode");
        assert_eq!(decoded.width(), 400);
    }

    #[test]
    fn resize_to_webp_rejects_garbage() {
        assert!(resize_to_webp(b"not an image", 100).is_err());
    }
}
