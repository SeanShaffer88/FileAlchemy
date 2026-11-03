//! In-process image conversions: PNG, JPG, WebP, AVIF, BMP, ICO, PDF.
//!
//! No external processes, no GPU, no network. SVG goes through `resvg` (pure
//! Rust rasteriser), AVIF through `ravif` (pure Rust rav1e encoder), PDF through
//! our own tiny writer that embeds the JPEG untouched.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat};

use crate::engine::formats::{Format, Preset, Resolution};
use crate::engine::mime::InputKind;
use crate::engine::{pdf_writer, CancelToken, EngineContext, EngineError, ProgressFn, Result, TaskRequest};

/// Largest bitmap we will try to hold in memory (RGBA, ~4 bytes/px).
const MAX_PIXELS: u64 = 250_000_000;

pub fn run(
    req: &TaskRequest,
    ctx: &EngineContext,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    progress(Some(0.02));

    let work = ctx.scratch.task_dir(req.id)?;
    let ext = req
        .target
        .extension()
        .ok_or_else(|| EngineError::Internal("image target without an extension".into()))?;
    let out_path = work.join(format!("result.{ext}"));

    let decoded = decode(req)?;
    if crate::engine::is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }
    progress(Some(0.35));

    let sized = apply_resolution(decoded, req.resolution);
    if crate::engine::is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }
    progress(Some(0.55));

    match req.target {
        Format::Png => encode_png(&sized, req.preset, &out_path)?,
        Format::Jpg => encode_jpeg(&sized, req.preset, &out_path)?,
        Format::Webp => encode_webp(&sized, &out_path)?,
        Format::Avif => encode_avif(&sized, req.preset, &out_path)?,
        Format::Bmp => encode_bmp(&sized, &out_path)?,
        Format::Ico => encode_ico(&sized, &out_path)?,
        Format::Pdf => encode_pdf(&sized, req.preset, &out_path)?,
        other => {
            return Err(EngineError::Unsupported {
                what: "image".into(),
                detail: format!("{} is not an image target", other.label()),
            })
        }
    }

    progress(Some(0.97));
    Ok(vec![out_path])
}

// -----------------------------------------------------------------------------
//  Decoding
// -----------------------------------------------------------------------------

fn decode(req: &TaskRequest) -> Result<DynamicImage> {
    if req.detected.kind == InputKind::Svg {
        return render_svg(&req.source);
    }

    let reader = image::ImageReader::open(&req.source)?
        .with_guessed_format()
        .map_err(|e| EngineError::Decode(e.to_string()))?;

    // Guard against decompression bombs before allocating the pixel buffer.
    if let Ok((w, h)) = reader.into_dimensions() {
        let pixels = u64::from(w) * u64::from(h);
        if pixels > MAX_PIXELS {
            return Err(EngineError::Decode(format!(
                "image is {w}x{h} ({} megapixels), larger than the {} megapixel safety limit",
                pixels / 1_000_000,
                MAX_PIXELS / 1_000_000
            )));
        }
    }

    let reader = image::ImageReader::open(&req.source)?
        .with_guessed_format()
        .map_err(|e| EngineError::Decode(e.to_string()))?;
    reader
        .decode()
        .map_err(|e| EngineError::Decode(format!("{}: {e}", req.file_name())))
}

/// Rasterise an SVG at its intrinsic size (capped), so the normal resize path
/// can then apply the user's resolution choice.
fn render_svg(path: &Path) -> Result<DynamicImage> {
    let data = std::fs::read(path)?;
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&data, &options)
        .map_err(|e| EngineError::Decode(format!("SVG could not be parsed: {e}")))?;

    let size = tree.size();
    let (mut width, mut height) = (size.width(), size.height());
    if !(width.is_finite() && height.is_finite()) || width < 0.5 || height < 0.5 {
        return Err(EngineError::Decode(
            "SVG has no usable intrinsic size (missing width/height/viewBox)".into(),
        ));
    }

    // Cap the raster so a pathological viewBox cannot exhaust memory.
    const MAX_SIDE: f32 = 8_000.0;
    let cap = (MAX_SIDE / width).min(MAX_SIDE / height).min(1.0);
    width *= cap;
    height *= cap;

    let px_w = width.round().max(1.0) as u32;
    let px_h = height.round().max(1.0) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(px_w, px_h)
        .ok_or_else(|| EngineError::Decode(format!("SVG is too large to rasterise ({px_w}x{px_h})")))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(px_w as f32 / size.width(), px_h as f32 / size.height()),
        &mut pixmap.as_mut(),
    );

    let png = pixmap
        .encode_png()
        .map_err(|e| EngineError::Decode(format!("SVG rasterisation failed: {e}")))?;
    image::load_from_memory_with_format(&png, ImageFormat::Png)
        .map_err(|e| EngineError::Decode(format!("SVG rasterisation failed: {e}")))
}

// -----------------------------------------------------------------------------
//  Resizing
// -----------------------------------------------------------------------------

fn apply_resolution(img: DynamicImage, resolution: Resolution) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    match resolution {
        Resolution::Original => img,
        Resolution::Half => {
            let nw = (w / 2).max(1);
            let nh = (h / 2).max(1);
            img.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
        }
        _ => match resolution.max_height() {
            Some(max_h) if h > max_h => {
                let scale = max_h as f32 / h as f32;
                let nw = ((w as f32 * scale).round() as u32).max(1);
                img.resize_exact(nw, max_h, image::imageops::FilterType::Lanczos3)
            }
            _ => img,
        },
    }
}

/// Icon containers only accept up to 256x256.
fn fit_icon(img: DynamicImage) -> DynamicImage {
    if img.width() > 256 || img.height() > 256 {
        img.resize(256, 256, image::imageops::FilterType::Lanczos3)
    } else {
        img
    }
}

/// Flatten transparency onto white, which is what users expect when a
/// transparent PNG becomes a JPG/BMP/PDF.
fn flatten_on_white(img: &DynamicImage) -> image::RgbImage {
    let rgba = img.to_rgba8();
    image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y);
        let alpha = f32::from(p[3]) / 255.0;
        let blend = |c: u8| -> u8 {
            ((f32::from(c) * alpha) + 255.0 * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8
        };
        image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])])
    })
}

// -----------------------------------------------------------------------------
//  Encoders
// -----------------------------------------------------------------------------

fn create(path: &Path) -> Result<BufWriter<File>> {
    Ok(BufWriter::new(File::create(path)?))
}

fn finish(mut writer: BufWriter<File>) -> Result<()> {
    // Flush to the OS, then ask the OS to put it on disk: the rename into place
    // must never expose a truncated file after a power loss.
    writer.flush()?;
    let file = writer
        .into_inner()
        .map_err(|e| EngineError::Encode(e.to_string()))?;
    file.sync_all()?;
    Ok(())
}

fn encode_png(img: &DynamicImage, preset: Preset, path: &Path) -> Result<()> {
    let compression = match preset {
        Preset::SmallerFile | Preset::Lossless => CompressionType::Best,
        Preset::HighQuality => CompressionType::Fast,
        Preset::Balanced => CompressionType::Default,
    };
    // 8-bit RGBA keeps transparency and is what every consumer expects.
    let rgba = img.to_rgba8();
    let mut writer = create(path)?;
    PngEncoder::new_with_quality(&mut writer, compression, FilterType::Adaptive)
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| EngineError::Encode(format!("PNG: {e}")))?;
    finish(writer)
}

fn encode_jpeg(img: &DynamicImage, preset: Preset, path: &Path) -> Result<()> {
    let rgb = flatten_on_white(img);
    let mut writer = create(path)?;
    JpegEncoder::new_with_quality(&mut writer, preset.image_quality())
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
        .map_err(|e| EngineError::Encode(format!("JPG: {e}")))?;
    finish(writer)
}

/// The `image` crate's WebP encoder is lossless-only. That is a deliberate
/// trade: it keeps the build 100% pure Rust (no libwebp C dependency to compile
/// or to ship), and lossless WebP is still ~30% smaller than PNG.
fn encode_webp(img: &DynamicImage, path: &Path) -> Result<()> {
    let rgba = img.to_rgba8();
    let mut writer = create(path)?;
    image::codecs::webp::WebPEncoder::new_lossless(&mut writer)
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| EngineError::Encode(format!("WebP: {e}")))?;
    finish(writer)
}

fn encode_avif(img: &DynamicImage, preset: Preset, path: &Path) -> Result<()> {
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);
    let pixels: Vec<rgb::RGBA8> = rgba
        .pixels()
        .map(|p| rgb::RGBA8::new(p[0], p[1], p[2], p[3]))
        .collect();

    let encoder = ravif::Encoder::new()
        .with_quality(preset.avif_quality())
        .with_alpha_quality(preset.avif_quality())
        .with_speed(preset.avif_speed());

    let encoded = encoder
        .encode_rgba(ravif::Img::new(&pixels[..], width, height))
        .map_err(|e| EngineError::Encode(format!("AVIF: {e}")))?;

    let mut writer = create(path)?;
    writer.write_all(&encoded.avif_file)?;
    finish(writer)
}

fn encode_bmp(img: &DynamicImage, path: &Path) -> Result<()> {
    let rgb = flatten_on_white(img);
    let mut writer = create(path)?;
    image::codecs::bmp::BmpEncoder::new(&mut writer)
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
        .map_err(|e| EngineError::Encode(format!("BMP: {e}")))?;
    finish(writer)
}

fn encode_ico(img: &DynamicImage, path: &Path) -> Result<()> {
    let icon = fit_icon(img.clone()).to_rgba8();
    let mut writer = create(path)?;
    image::codecs::ico::IcoEncoder::new(&mut writer)
        .write_image(
            icon.as_raw(),
            icon.width(),
            icon.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| EngineError::Encode(format!("ICO: {e}")))?;
    finish(writer)
}

fn encode_pdf(img: &DynamicImage, preset: Preset, path: &Path) -> Result<()> {
    // Embed a high-quality JPEG: the PDF is a container, the compression is the
    // JPEG's, so the preset maps straight onto its quality.
    let rgb = flatten_on_white(img);
    let mut jpeg: Vec<u8> = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, preset.image_quality().max(80))
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
        .map_err(|e| EngineError::Encode(format!("PDF/JPEG: {e}")))?;

    let mut writer = create(path)?;
    pdf_writer::write_single_page_jpeg(&mut writer, &jpeg, rgb.width(), rgb.height())
        .map_err(|e| EngineError::Encode(format!("PDF: {e}")))?;
    finish(writer)
}

/// Kept for callers that want a one-shot encode into memory (tests).
pub fn encode_to_memory(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, format)
        .map_err(|e| EngineError::Encode(e.to_string()))?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DynamicImage {
        // 40x20 with a transparent square, so alpha handling is exercised.
        let mut img = image::RgbaImage::new(40, 20);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = if x < 10 {
                image::Rgba([255, 0, 0, 0])
            } else {
                image::Rgba([10, 120, 200, 255])
            };
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn resolution_half_halves_both_sides() {
        let out = apply_resolution(sample(), Resolution::Half);
        assert_eq!((out.width(), out.height()), (20, 10));
    }

    #[test]
    fn resolution_720_only_shrinks() {
        let small = apply_resolution(sample(), Resolution::P720);
        assert_eq!((small.width(), small.height()), (40, 20));
    }

    #[test]
    fn alpha_is_flattened_to_white() {
        let rgb = flatten_on_white(&sample());
        let transparent = rgb.get_pixel(0, 0);
        assert_eq!((transparent[0], transparent[1], transparent[2]), (255, 255, 255));
        let opaque = rgb.get_pixel(20, 5);
        assert_eq!((opaque[0], opaque[1], opaque[2]), (10, 120, 200));
    }

    #[test]
    fn icons_are_clamped_to_256() {
        let big = DynamicImage::ImageRgba8(image::RgbaImage::new(1024, 512));
        let icon = fit_icon(big);
        assert!(icon.width() <= 256 && icon.height() <= 256);
    }

    #[test]
    fn png_roundtrip_preserves_size() {
        let dir = std::env::temp_dir().join(format!("filealchemy-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.png");
        encode_png(&sample(), Preset::Balanced, &path).unwrap();
        let decoded = image::open(&path).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (40, 20));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
