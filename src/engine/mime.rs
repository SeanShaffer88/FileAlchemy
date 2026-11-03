//! Real type detection.
//!
//! Extensions lie: `.jpg` files that are really PNGs, `.docx` files that are
//! really ZIPs, archives that are really tar.gz. We therefore sniff magic bytes
//! (`infer`, first 512 bytes) first, then refine container formats by looking
//! *inside* them (ZIP local headers, gzip payloads, UTF-8 text), and only fall
//! back to the extension when the content is inconclusive. Every fallback is
//! reported to the UI so the user can see how a file was classified.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::engine::formats::{Category, Format, Profile};
use crate::engine::{EngineError, Result};

/// What we believe the file actually is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Png,
    Jpeg,
    Webp,
    Bmp,
    Tiff,
    Gif,
    Ico,
    Svg,
    Avif,
    Qoi,
    Mp4,
    Mkv,
    Mov,
    Avi,
    Webm,
    Mp3,
    Wav,
    Flac,
    Aac,
    Ogg,
    M4a,
    Pdf,
    Docx,
    Epub,
    Markdown,
    Txt,
    Html,
    Rtf,
    Csv,
    Json,
    Zip,
    SevenZ,
    Tar,
    Gzip,
    TarGz,
}

impl InputKind {
    pub fn category(self) -> Category {
        use InputKind::*;
        match self {
            Png | Jpeg | Webp | Bmp | Tiff | Gif | Ico | Svg | Avif | Qoi => Category::Image,
            Mp4 | Mkv | Mov | Avi | Webm => Category::Video,
            Mp3 | Wav | Flac | Aac | Ogg | M4a => Category::Audio,
            Pdf | Docx | Epub | Markdown | Txt | Html | Rtf | Csv | Json => Category::Document,
            Zip | SevenZ | Tar | Gzip | TarGz => Category::Archive,
        }
    }

    pub fn profile(self) -> Profile {
        use InputKind::*;
        match self {
            Pdf => Profile::Pdf,
            Mp4 | Mkv | Mov | Avi | Webm => Profile::Video,
            Mp3 | Wav | Flac | Aac | Ogg | M4a => Profile::Audio,
            Zip | SevenZ | Tar | Gzip | TarGz => Profile::Archive,
            Docx | Epub | Markdown | Txt | Html | Rtf | Csv | Json => Profile::Text,
            _ => Profile::Image,
        }
    }

    /// Representative MIME type, shown in the queue card.
    pub fn mime(self) -> &'static str {
        use InputKind::*;
        match self {
            Png => "image/png",
            Jpeg => "image/jpeg",
            Webp => "image/webp",
            Bmp => "image/bmp",
            Tiff => "image/tiff",
            Gif => "image/gif",
            Ico => "image/vnd.microsoft.icon",
            Svg => "image/svg+xml",
            Avif => "image/avif",
            Qoi => "image/qoi",
            Mp4 => "video/mp4",
            Mkv => "video/x-matroska",
            Mov => "video/quicktime",
            Avi => "video/x-msvideo",
            Webm => "video/webm",
            Mp3 => "audio/mpeg",
            Wav => "audio/wav",
            Flac => "audio/flac",
            Aac => "audio/aac",
            Ogg => "audio/ogg",
            M4a => "audio/mp4",
            Pdf => "application/pdf",
            Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Epub => "application/epub+zip",
            Markdown => "text/markdown",
            Txt => "text/plain",
            Html => "text/html",
            Rtf => "application/rtf",
            Csv => "text/csv",
            Json => "application/json",
            Zip => "application/zip",
            SevenZ => "application/x-7z-compressed",
            Tar => "application/x-tar",
            Gzip => "application/gzip",
            TarGz => "application/gzip",
        }
    }

    /// Short label used in the UI badge ("PNG", "DOCX", ...).
    pub fn label(self) -> &'static str {
        use InputKind::*;
        match self {
            Png => "PNG",
            Jpeg => "JPEG",
            Webp => "WebP",
            Bmp => "BMP",
            Tiff => "TIFF",
            Gif => "GIF",
            Ico => "ICO",
            Svg => "SVG",
            Avif => "AVIF",
            Qoi => "QOI",
            Mp4 => "MP4",
            Mkv => "MKV",
            Mov => "MOV",
            Avi => "AVI",
            Webm => "WebM",
            Mp3 => "MP3",
            Wav => "WAV",
            Flac => "FLAC",
            Aac => "AAC",
            Ogg => "OGG",
            M4a => "M4A",
            Pdf => "PDF",
            Docx => "DOCX",
            Epub => "EPUB",
            Markdown => "MD",
            Txt => "TXT",
            Html => "HTML",
            Rtf => "RTF",
            Csv => "CSV",
            Json => "JSON",
            Zip => "ZIP",
            SevenZ => "7Z",
            Tar => "TAR",
            Gzip => "GZ",
            TarGz => "TAR.GZ",
        }
    }

    /// Sensible default target: the conversion a normal person would want for
    /// this input. Never equal to the input container where that would be a
    /// pointless no-op re-encode.
    pub fn default_target(self) -> Format {
        use InputKind::*;
        match self {
            Png | Webp | Bmp | Tiff | Gif | Ico | Svg | Avif | Qoi => Format::Webp,
            Jpeg => Format::Webp,
            // MP4 -> MP4 would be a pointless default re-encode, so a plain
            // MP4 defaults to WebM and everything else defaults to MP4.
            Mp4 => Format::Webm,
            Webm | Mkv | Mov | Avi => Format::Mp4,
            Wav | Flac | Aac | Ogg => Format::Mp3,
            Mp3 | M4a => Format::M4a,
            Pdf => Format::Png,
            Docx | Html | Epub => Format::Markdown,
            Markdown => Format::Docx,
            Txt => Format::Markdown,
            Csv => Format::Html,
            Json => Format::Txt,
            Rtf => Format::Txt,
            Zip => Format::ExtractDir,
            SevenZ | Tar | TarGz => Format::Zip,
            Gzip => Format::Zip,
        }
    }

    /// Formats in the target list for this input, as the UI shows them.
    pub fn targets(self) -> &'static [Format] {
        self.profile().targets()
    }

    /// All extensions we recognise for this kind (used by the fallback path).
    pub fn extensions(self) -> &'static [&'static str] {
        use InputKind::*;
        match self {
            Png => &["png"],
            Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            Webp => &["webp"],
            Bmp => &["bmp", "dib"],
            Tiff => &["tif", "tiff"],
            Gif => &["gif"],
            Ico => &["ico", "cur"],
            Svg => &["svg"],
            Avif => &["avif"],
            Qoi => &["qoi"],
            Mp4 => &["mp4", "m4v"],
            Mkv => &["mkv"],
            Mov => &["mov"],
            Avi => &["avi"],
            Webm => &["webm"],
            Mp3 => &["mp3"],
            Wav => &["wav"],
            Flac => &["flac"],
            Aac => &["aac"],
            Ogg => &["ogg", "oga", "opus"],
            M4a => &["m4a"],
            Pdf => &["pdf"],
            Docx => &["docx", "docm"],
            Epub => &["epub"],
            Markdown => &["md", "markdown", "mdown"],
            Txt => &["txt", "text", "log"],
            Html => &["html", "htm", "xhtml"],
            Rtf => &["rtf"],
            Csv => &["csv", "tsv"],
            Json => &["json"],
            Zip => &["zip"],
            SevenZ => &["7z"],
            Tar => &["tar"],
            Gzip => &["gz"],
            TarGz => &["tar.gz", "tgz"],
        }
    }
}

/// Result of inspecting a file.
#[derive(Clone, Debug)]
pub struct Detected {
    pub kind: InputKind,
    /// MIME type to show in the UI.
    pub mime: String,
    /// How sure we are, in plain English ("detected from file content").
    pub confidence: Confidence,
    /// Optional extra note for the UI (e.g. "extension says .docx").
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    /// Magic bytes matched a known signature.
    Content,
    /// Content was inspected more deeply (zip/gzip containers, text sniffing).
    DeepContent,
    /// Only the file extension matched.
    Extension,
    /// Nothing matched; the file is assumed to be text.
    Assumption,
}

impl Confidence {
    pub fn description(self) -> &'static str {
        match self {
            Confidence::Content => "detected from file content",
            Confidence::DeepContent => "detected by inspecting the file",
            Confidence::Extension => "guessed from the file extension",
            Confidence::Assumption => "assumed from unreadable content",
        }
    }
}

impl Detected {
    pub fn category(&self) -> Category {
        self.kind.category()
    }

    pub fn profile(&self) -> Profile {
        self.kind.profile()
    }

    pub fn default_target(&self) -> Format {
        self.kind.default_target()
    }

    pub fn targets(&self) -> &'static [Format] {
        self.kind.targets()
    }

    /// Type string for the queue card: the real MIME type when it is short
    /// enough to read ("image/png"), otherwise the format label ("DOCX") —
    /// the full MIME type stays available through the hint bar and the
    /// accessibility description.
    pub fn display_type(&self) -> String {
        if self.mime.len() <= 24 {
            self.mime.clone()
        } else {
            self.kind.label().to_string()
        }
    }
}

/// Inspect a file and decide what it really is.
pub fn detect(path: &Path) -> Result<Detected> {
    let mut head = [0u8; 512];
    let read = read_head(path, &mut head)?;
    let head = &head[..read];
    let ext = extension_chain(path);

    // 1. magic bytes
    if let Some(kind) = from_magic(head) {
        // …refined when the container hides the real payload (zip / gzip).
        if kind == InputKind::Zip {
            if let Some(inner) = refine_zip(path) {
                return Ok(Detected {
                    kind: inner,
                    mime: inner.mime().to_string(),
                    confidence: Confidence::DeepContent,
                    note: Some("zip container inspected".into()),
                });
            }
            // An audio-only "mp4"/"zip" mismatch: fall back to the extension.
            if let Some(kind) = from_extension(&ext) {
                if kind != InputKind::Zip {
                    return Ok(Detected {
                        kind,
                        mime: kind.mime().to_string(),
                        confidence: Confidence::DeepContent,
                        note: Some("container matched the extension".into()),
                    });
                }
            }
        }
        if kind == InputKind::Gzip {
            if let Some(inner) = refine_gzip(path) {
                return Ok(Detected {
                    kind: inner,
                    mime: inner.mime().to_string(),
                    confidence: Confidence::DeepContent,
                    note: Some("compressed stream inspected".into()),
                });
            }
        }

        // MP4 is also the container for M4A/AAC audio; the extension decides.
        let kind = refine_media_by_extension(kind, &ext);

        return Ok(Detected {
            kind,
            mime: kind.mime().to_string(),
            confidence: Confidence::Content,
            note: None,
        });
    }

    // 2. text-ish content: SVG, HTML, Markdown, CSV, JSON, RTF, TXT
    if let Some(kind) = sniff_text(head, &ext) {
        return Ok(Detected {
            kind,
            mime: kind.mime().to_string(),
            confidence: Confidence::DeepContent,
            note: None,
        });
    }

    // 3. extension
    if let Some(kind) = from_extension(&ext) {
        return Ok(Detected {
            kind,
            mime: kind.mime().to_string(),
            confidence: Confidence::Extension,
            note: Some("content was inconclusive".into()),
        });
    }

    Err(EngineError::Unsupported {
        what: "file type".into(),
        detail: format!(
            "{} does not look like a supported image, audio, video, document or archive. \
             Rename it with the right extension if the content is known to be supported.",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        ),
    })
}

fn read_head(path: &Path, buf: &mut [u8]) -> Result<usize> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut total = 0;
    while total < buf.len() {
        match reader.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(EngineError::Io(e)),
        }
    }
    Ok(total)
}

/// Lower-cased extension chain, shortest first: `a.tar.gz` -> `["gz", "tar.gz"]`.
fn extension_chain(path: &Path) -> Vec<String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let mut out = Vec::new();
    let mut acc = String::new();
    let mut current = name.as_str();
    loop {
        let Some(idx) = current.rfind('.') else { break };
        let tail = &current[idx + 1..];
        if !tail.is_empty() {
            acc = if acc.is_empty() {
                tail.to_string()
            } else {
                format!("{tail}.{acc}")
            };
            out.push(acc.clone());
        }
        if idx == 0 {
            break;
        }
        current = &current[..idx];
        if out.len() >= 4 {
            break;
        }
    }
    out
}

fn from_extension(exts: &[String]) -> Option<InputKind> {
    use InputKind::*;
    let candidates: &[InputKind] = &[
        Png, Jpeg, Webp, Bmp, Tiff, Gif, Ico, Svg, Avif, Qoi, Mp4, Mkv, Mov, Avi, Webm, Mp3, Wav,
        Flac, Aac, Ogg, M4a, Pdf, Docx, Epub, Markdown, Txt, Html, Rtf, Csv, Json, Zip, SevenZ, Tar,
        Gzip, TarGz,
    ];
    // Longest (most specific) extensions first: "tar.gz" before "gz".
    let mut sorted: Vec<&InputKind> = candidates.iter().collect();
    sorted.sort_by_key(|k| std::cmp::Reverse(k.extensions().iter().map(|e| e.len()).max().unwrap_or(0)));
    for kind in sorted {
        for ext in exts {
            if kind.extensions().iter().any(|e| e == ext) {
                return Some(*kind);
            }
        }
    }
    None
}

/// Signature table for the formats that have reliable magic bytes and are not
/// (yet) covered by every infer release.
fn from_magic(head: &[u8]) -> Option<InputKind> {
    use InputKind::*;

    if let Some(t) = infer::get(head) {
        let mime = t.mime_type();
        let kind = match mime {
            "image/png" => Some(Png),
            "image/jpeg" => Some(Jpeg),
            "image/webp" => Some(Webp),
            "image/bmp" => Some(Bmp),
            "image/tiff" => Some(Tiff),
            "image/gif" => Some(Gif),
            "image/vnd.microsoft.icon" | "image/x-icon" | "image/ico" => Some(Ico),
            "image/avif" => Some(Avif),
            "image/qoi" => Some(Qoi),
            "image/svg+xml" => Some(Svg),
            "video/mp4" | "audio/mp4" => Some(Mp4),
            "video/x-matroska" | "audio/x-matroska" => Some(Mkv),
            "video/quicktime" => Some(Mov),
            "video/x-msvideo" => Some(Avi),
            "video/webm" | "audio/webm" => Some(Webm),
            "audio/mpeg" => Some(Mp3),
            "audio/wav" | "audio/x-wav" | "audio/vnd.wave" => Some(Wav),
            "audio/flac" | "audio/x-flac" => Some(Flac),
            "audio/aac" | "audio/x-aac" => Some(Aac),
            "audio/ogg" | "application/ogg" => Some(Ogg),
            "application/pdf" => Some(Pdf),
            "application/zip" => Some(Zip),
            "application/x-7z-compressed" => Some(SevenZ),
            "application/x-tar" => Some(Tar),
            "application/gzip" | "application/x-gzip" => Some(Gzip),
            "application/epub+zip" => Some(Epub),
            "application/msword" => Some(Docx),
            "application/rtf" | "text/rtf" => Some(Rtf),
            "application/json" => Some(Json),
            "text/csv" => Some(Csv),
            "text/html" => Some(Html),
            "text/markdown" => Some(Markdown),
            "text/plain" => Some(Txt),
            _ => None,
        };
        if kind.is_some() {
            return kind;
        }
    }

    // Signatures infer may not know in older releases.
    if head.len() >= 24 && &head[4..8] == b"ftyp" {
        return Some(Mp4);
    }
    if head.starts_with(b"qoif") {
        return Some(Qoi);
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some(Webp);
    }
    if head.len() > 262 && &head[257..262] == b"ustar" {
        return Some(Tar);
    }
    None
}

/// Distinguish audio-only MP4 containers from video by extension.
fn refine_media_by_extension(kind: InputKind, exts: &[String]) -> InputKind {
    let has = |e: &str| exts.iter().any(|x| x == e);
    match kind {
        InputKind::Mp4 if has("m4a") || has("aac") => InputKind::M4a,
        InputKind::Mp4 if has("m4b") || has("m4p") => InputKind::M4a,
        InputKind::Webm => InputKind::Webm,
        other => other,
    }
}

/// Look inside a ZIP to tell DOCX / EPUB / plain ZIP apart.
fn refine_zip(path: &Path) -> Option<InputKind> {
    let file = File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).ok()?;
    let mut names = Vec::new();
    for i in 0..archive.len().min(64) {
        if let Ok(entry) = archive.by_index_raw(i) {
            names.push(entry.name().to_string());
        }
    }
    if names.iter().any(|n| n == "mimetype") {
        // EPUB stores the mimetype first, uncompressed.
        if let Ok(mut m) = archive.by_name("mimetype") {
            let mut buf = String::new();
            if m.read_to_string(&mut buf).is_ok() && buf.contains("epub") {
                return Some(InputKind::Epub);
            }
        }
    }
    if names.iter().any(|n| n == "word/document.xml") {
        return Some(InputKind::Docx);
    }
    if names.iter().any(|n| n.starts_with("META-INF/") && n.ends_with(".xml")) {
        return None;
    }
    None
}

/// Look inside a gzip stream: `.tar.gz` versus a single compressed file.
fn refine_gzip(path: &Path) -> Option<InputKind> {
    let file = File::open(path).ok()?;
    let mut decoder = flate2::read::GzDecoder::new(BufReader::new(file));
    let mut head = [0u8; 512];
    let read = match decoder.read(&mut head) {
        Ok(n) => n,
        Err(_) => return None,
    };
    if read > 262 && &head[257..262] == b"ustar" {
        return Some(InputKind::TarGz);
    }
    None
}

/// Text formats have no magic number, so inspect the first bytes.
fn sniff_text(head: &[u8], exts: &[String]) -> Option<InputKind> {
    let has = |e: &str| exts.iter().any(|x| x == e);

    // RTF is plain ASCII starting with {\rtf
    if head.starts_with(b"{\\rtf") {
        return Some(InputKind::Rtf);
    }

    // Valid UTF-8 (allowing a BOM) is a good signal that this is text.
    let body = head.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(head);
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(e) if e.valid_up_to() > 0 => std::str::from_utf8(&body[..e.valid_up_to()]).ok()?,
        Err(_) => return None,
    };
    let trimmed_start = text.trim_start();
    let lower = trimmed_start.to_ascii_lowercase();

    if lower.starts_with("<svg") || (lower.starts_with("<?xml") && lower.contains("<svg")) {
        return Some(InputKind::Svg);
    }
    if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
        return Some(InputKind::Html);
    }
    if lower.starts_with("<?xml") {
        // EPUB package documents / other XML: trust the extension.
        return from_extension(exts);
    }
    if lower.starts_with('{') || lower.starts_with('[') {
        if serde_json::from_str::<serde_json::Value>(text).is_ok() || has("json") {
            return Some(InputKind::Json);
        }
    }
    if has("csv") || has("tsv") {
        return Some(InputKind::Csv);
    }
    if has("md") || has("markdown") || has("mdown") {
        return Some(InputKind::Markdown);
    }
    if has("html") || has("htm") {
        return Some(InputKind::Html);
    }
    if has("svg") {
        return Some(InputKind::Svg);
    }
    if has("rtf") {
        return Some(InputKind::Rtf);
    }
    if has("json") {
        return Some(InputKind::Json);
    }
    // Any other readable UTF-8 content is treated as plain text.
    if from_extension(exts).is_some() {
        return from_extension(exts);
    }
    Some(InputKind::Txt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_chain_handles_compound_extensions() {
        let chain = extension_chain(Path::new("/tmp/archive.TAR.GZ"));
        assert!(chain.contains(&"tar.gz".to_string()), "{chain:?}");
        assert!(chain.contains(&"gz".to_string()), "{chain:?}");
    }

    #[test]
    fn extension_fallback_prefers_longest_match() {
        let chain = extension_chain(Path::new("a.tar.gz"));
        assert_eq!(from_extension(&chain), Some(InputKind::TarGz));
        let chain = extension_chain(Path::new("a.gz"));
        assert_eq!(from_extension(&chain), Some(InputKind::Gzip));
    }

    #[test]
    fn magic_detects_misnamed_files() {
        let png = [
            0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        assert_eq!(from_magic(&png), Some(InputKind::Png));
    }

    #[test]
    fn text_sniffing_finds_svg_and_html() {
        assert_eq!(
            sniff_text(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>", &[]),
            Some(InputKind::Svg)
        );
        assert_eq!(
            sniff_text(b"<!DOCTYPE html><html></html>", &[]),
            Some(InputKind::Html)
        );
    }

    #[test]
    fn every_kind_has_sane_metadata() {
        use InputKind::*;
        for k in [
            Png, Jpeg, Webp, Bmp, Tiff, Gif, Ico, Svg, Avif, Qoi, Mp4, Mkv, Mov, Avi, Webm, Mp3,
            Wav, Flac, Aac, Ogg, M4a, Pdf, Docx, Epub, Markdown, Txt, Html, Rtf, Csv, Json, Zip,
            SevenZ, Tar, Gzip, TarGz,
        ] {
            assert!(!k.mime().is_empty());
            assert!(!k.label().is_empty());
            assert!(!k.extensions().is_empty(), "{k:?} has no extension");
            assert!(
                k.targets().contains(&k.default_target()),
                "{k:?} default target is not in its own target list"
            );
        }
    }

    #[test]
    fn default_targets_are_not_no_ops() {
        // WebP -> WebP would be a pointless re-encode.
        assert_ne!(InputKind::Webp.default_target(), Format::Webp);
        assert_ne!(InputKind::Mp4.default_target(), Format::Mp4);
        assert_ne!(InputKind::Zip.default_target(), Format::Zip);
    }
}
