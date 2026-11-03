//! Minimal single-page PDF writer.
//!
//! Why hand-rolled: the only PDF output FileAlchemy offers is "turn this image
//! into a one-page PDF", and pulling in a PDF library (plus its C dependencies)
//! for that would break the "single static executable, offline build" rule. A
//! PDF that embeds one JPEG in a page is ~2 KB of structure, so we write the
//! bytes ourselves. The JPEG is embedded with `/DCTDecode`, i.e. the image data
//! is passed through untouched — no recompression, no quality loss.

use std::io::{self, Write};

/// Render a 1-pixel-per-point page at a 96 dpi CSS assumption.
const PT_PER_PX: f64 = 72.0 / 96.0;

/// Write a single-page PDF containing `jpeg` (baseline JPEG, 8-bit RGB).
pub fn write_single_page_jpeg<W: Write>(
    out: &mut W,
    jpeg: &[u8],
    width_px: u32,
    height_px: u32,
) -> io::Result<()> {
    let width_pt = (width_px as f64 * PT_PER_PX).max(1.0);
    let height_pt = (height_px as f64 * PT_PER_PX).max(1.0);

    let content = format!(
        "q\n{w:.2} 0 0 {h:.2} 0 0 cm\n/Im0 Do\nQ\n",
        w = width_pt,
        h = height_pt
    );

    let mut offsets: Vec<usize> = Vec::with_capacity(6);
    let mut buf: Vec<u8> = Vec::with_capacity(jpeg.len() + 1024);

    // Header. The %âãÏÓ comment marks the file as containing binary data, which
    // is what Windows' GDI+ / Edge PDF viewers expect.
    buf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    fn begin_object(buf: &[u8], offsets: &mut Vec<usize>) {
        offsets.push(buf.len());
    }

    // 1: catalog
    begin_object(&buf, &mut offsets);
    buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // 2: page tree
    begin_object(&buf, &mut offsets);
    buf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    // 3: page
    begin_object(&buf, &mut offsets);
    buf.extend_from_slice(
        format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w:.2} {h:.2}] \
             /Resources << /XObject << /Im0 4 0 R >> /ProcSet [/PDF /ImageC] >> \
             /Contents 5 0 R >>\nendobj\n",
            w = width_pt,
            h = height_pt
        )
        .as_bytes(),
    );

    // 4: the image XObject (DCTDecode = the JPEG we were given)
    begin_object(&buf, &mut offsets);
    buf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /XObject /Subtype /Image /Width {w} /Height {h} \
             /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Length {len} >>\nstream\n",
            w = width_px,
            h = height_px,
            len = jpeg.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(jpeg);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    // 5: content stream
    begin_object(&buf, &mut offsets);
    buf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Length {len} >>\nstream\n{content}endstream\nendobj\n",
            len = content.len()
        )
        .as_bytes(),
    );

    let xref_offset = buf.len();
    let count = offsets.len() + 1; // + the free object 0
    buf.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f\r\n").as_bytes());
    for offset in &offsets {
        buf.extend_from_slice(format!("{offset:010} 00000 n\r\n").as_bytes());
    }
    buf.extend_from_slice(
        format!(
            "trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
        )
        .as_bytes(),
    );

    out.write_all(&buf)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_a_structurally_valid_pdf() {
        // Tiny but real JPEG header so the DCTDecode stream is plausible.
        let jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0xFF, 0xD9];
        let mut out = Vec::new();
        write_single_page_jpeg(&mut out, &jpeg, 640, 480).unwrap();

        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.trim_end().ends_with("%%EOF"));
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/Filter /DCTDecode"));
        assert!(text.contains("/Width 640"));
        assert!(text.contains("/Height 480"));
        assert!(text.contains("xref"));
        assert!(text.contains("trailer"));

        // Every xref offset must point at the matching "N 0 obj" header.
        let xref_at = text.find("\nxref\n").expect("xref table") + 1;
        let table = &text[xref_at..];
        let mut lines = table.lines();
        assert_eq!(lines.next(), Some("xref"));
        let header = lines.next().unwrap();
        let count: usize = header.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert_eq!(count, 6);
        assert_eq!(lines.next().unwrap(), "0000000000 65535 f");
        for i in 1..count {
            let entry = lines.next().unwrap();
            let offset: usize = entry.split_whitespace().next().unwrap().parse().unwrap();
            let expect = format!("{i} 0 obj");
            assert!(
                out[offset..].starts_with(expect.as_bytes()),
                "object {i} offset {offset} does not point at {expect}"
            );
        }
    }

    #[test]
    fn page_fits_inside_media_box() {
        let mut out = Vec::new();
        write_single_page_jpeg(&mut out, &[0xFF, 0xD8, 0xFF, 0xD9], 1920, 1080).unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        // 1920px at 96dpi = 20 inches = 1440pt
        assert!(text.contains("/MediaBox [0 0 1440.00 810.00]"), "{text}");
        assert!(text.contains("1440.00 0 0 810.00 0 0 cm"));
    }
}
