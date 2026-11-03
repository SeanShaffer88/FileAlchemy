//! Format matrix: the single source of truth for "what can become what".
//!
//! Both the UI (via `Options` in appwindow.slint) and the conversion engine
//! read these tables, so a format can never appear in the UI that the engine
//! cannot actually produce.

use serde::{Deserialize, Serialize};

/// Broad families used for grouping, badges and routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    Image,
    Video,
    Audio,
    Document,
    Archive,
}

impl Category {
    pub fn id(self) -> &'static str {
        match self {
            Category::Image => "image",
            Category::Video => "video",
            Category::Audio => "audio",
            Category::Document => "document",
            Category::Archive => "archive",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Category::Image => "Image",
            Category::Video => "Video",
            Category::Audio => "Audio",
            Category::Document => "Document",
            Category::Archive => "Archive",
        }
    }
}

/// Every target the application can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    // images (in-process Rust)
    Png,
    Jpg,
    Webp,
    Avif,
    Bmp,
    Ico,
    Pdf,
    // audio / video (bundled ffmpeg.exe)
    Mp4,
    Webm,
    Gif,
    Mp3,
    Wav,
    Flac,
    M4a,
    // documents (bundled pandoc.exe / pdftoppm.exe)
    Docx,
    Markdown,
    Html,
    Txt,
    Epub,
    // archives (in-process Rust)
    Zip,
    SevenZ,
    TarGz,
    ExtractDir,
}

pub const IMAGE_TARGETS: &[Format] = &[
    Format::Png,
    Format::Jpg,
    Format::Webp,
    Format::Avif,
    Format::Bmp,
    Format::Ico,
    Format::Pdf,
];

pub const VIDEO_TARGETS: &[Format] = &[
    Format::Mp4,
    Format::Webm,
    Format::Gif,
    Format::Mp3,
    Format::Wav,
    Format::Flac,
    Format::M4a,
];

pub const AUDIO_TARGETS: &[Format] = &[Format::Mp3, Format::Wav, Format::Flac, Format::M4a];

/// Markdown / HTML / TXT / DOCX / EPUB producers/consumers.
pub const TEXT_TARGETS: &[Format] = &[
    Format::Docx,
    Format::Markdown,
    Format::Html,
    Format::Txt,
    Format::Epub,
];

/// A PDF can only be rasterised, because pandoc cannot read PDF and no
/// LaTeX engine is bundled (offline, no network installs).
pub const PDF_TARGETS: &[Format] = &[Format::Png];

pub const ARCHIVE_TARGETS: &[Format] = &[
    Format::Zip,
    Format::SevenZ,
    Format::TarGz,
    Format::ExtractDir,
];

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpg => "JPG",
            Format::Webp => "WebP",
            Format::Avif => "AVIF",
            Format::Bmp => "BMP",
            Format::Ico => "ICO",
            Format::Pdf => "PDF",
            Format::Mp4 => "MP4",
            Format::Webm => "WebM",
            Format::Gif => "GIF",
            Format::Mp3 => "MP3",
            Format::Wav => "WAV",
            Format::Flac => "FLAC",
            Format::M4a => "M4A",
            Format::Docx => "DOCX",
            Format::Markdown => "Markdown",
            Format::Html => "HTML",
            Format::Txt => "TXT",
            Format::Epub => "EPUB",
            Format::Zip => "ZIP",
            Format::SevenZ => "7Z",
            Format::TarGz => "TAR.GZ",
            Format::ExtractDir => "Extract to folder",
        }
    }

    /// File extension without the dot. `None` for the extract-to-folder target.
    pub fn extension(self) -> Option<&'static str> {
        Some(match self {
            Format::Png => "png",
            Format::Jpg => "jpg",
            Format::Webp => "webp",
            Format::Avif => "avif",
            Format::Bmp => "bmp",
            Format::Ico => "ico",
            Format::Pdf => "pdf",
            Format::Mp4 => "mp4",
            Format::Webm => "webm",
            Format::Gif => "gif",
            Format::Mp3 => "mp3",
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::M4a => "m4a",
            Format::Docx => "docx",
            Format::Markdown => "md",
            Format::Html => "html",
            Format::Txt => "txt",
            Format::Epub => "epub",
            Format::Zip => "zip",
            Format::SevenZ => "7z",
            Format::TarGz => "tar.gz",
            Format::ExtractDir => return None,
        })
    }

    /// Plain-English explanation, surfaced by the hint bar / accessible name.
    pub fn help(self) -> &'static str {
        match self {
            Format::Png => "Lossless image format with transparency support. Bigger than JPG/WebP but never degrades when re-saved.",
            Format::Jpg => "The most universal photo format. Lossy, small files; best for photographs, no transparency.",
            Format::Webp => "A modern web image format created by Google. Yields about 30% smaller files than PNG while keeping transparency.",
            Format::Avif => "Next-generation image format with excellent compression. Smaller than WebP, but older software may not open it.",
            Format::Bmp => "Uncompressed bitmap. Huge files, but readable by literally everything, including old lab software.",
            Format::Ico => "Windows icon container. Multi-size ready, used for shortcuts and folders.",
            Format::Pdf => "A fixed-layout document that looks identical everywhere. Ideal for printing and sharing.",
            Format::Mp4 => "The most compatible video format. Plays on all phones, browsers, TVs and Windows PCs without extra software.",
            Format::Webm => "Open video format (VP9 + Opus). Smaller than MP4 and royalty free; plays in browsers, some TVs need it converted.",
            Format::Gif => "Short, silent, looping animation. Huge files and 256 colours - use MP4 when quality matters.",
            Format::Mp3 => "The universal audio format. Plays everywhere, small files.",
            Format::Wav => "Uncompressed audio. Perfect fidelity for editing, but roughly 10 MB per minute.",
            Format::Flac => "Lossless audio compression. Preserves 100% of the original studio sound quality at about half the WAV size.",
            Format::M4a => "AAC audio in an MP4 container: the same quality as iTunes downloads, smaller than MP3.",
            Format::Docx => "Microsoft Word document. Editable in Word, LibreOffice and Google Docs.",
            Format::Markdown => "Plain-text with simple formatting (# headings, **bold**). Ideal for notes, READMEs and version control.",
            Format::Html => "Web page. Opens in any browser, easy to publish or e-mail.",
            Format::Txt => "Plain text with no formatting. Universal, tiny, opens anywhere.",
            Format::Epub => "E-book format for Kobo, Apple Books and most readers (Kindle needs a conversion).",
            Format::Zip => "The most widely supported archive. Opens without extra software on Windows, macOS and Linux.",
            Format::SevenZ => "Highest-ratio archive format. Smaller than ZIP; needs 7-Zip to open on some machines.",
            Format::TarGz => "Unix-style archive: files collected then gzip-compressed. Standard for Linux and macOS.",
            Format::ExtractDir => "Unpack the archive into a folder instead of creating a new archive file.",
        }
    }

    /// True when producing this target requires ffmpeg.exe.
    pub fn needs_ffmpeg(self) -> bool {
        matches!(
            self,
            Format::Mp4 | Format::Webm | Format::Gif | Format::Mp3 | Format::Wav | Format::Flac | Format::M4a
        )
    }

    /// True when producing this target requires pandoc.exe.
    pub fn needs_pandoc(self) -> bool {
        matches!(
            self,
            Format::Docx | Format::Markdown | Format::Html | Format::Txt | Format::Epub
        )
    }

    /// Look a format up by its UI label (used by tests and future CLI work).
    #[allow(dead_code)]
    pub fn from_label(label: &str) -> Option<Format> {
        ALL.iter().copied().find(|f| f.label() == label)
    }
}

pub const ALL: &[Format] = &[
    Format::Png,
    Format::Jpg,
    Format::Webp,
    Format::Avif,
    Format::Bmp,
    Format::Ico,
    Format::Pdf,
    Format::Mp4,
    Format::Webm,
    Format::Gif,
    Format::Mp3,
    Format::Wav,
    Format::Flac,
    Format::M4a,
    Format::Docx,
    Format::Markdown,
    Format::Html,
    Format::Txt,
    Format::Epub,
    Format::Zip,
    Format::SevenZ,
    Format::TarGz,
    Format::ExtractDir,
];

/// Identifies which *target list* applies, so the UI can show a per-item,
/// accurate list without shipping one array per input format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Image,
    Video,
    Audio,
    Text,
    Pdf,
    Archive,
}

impl Profile {
    pub fn id(self) -> &'static str {
        match self {
            Profile::Image => "image",
            Profile::Video => "video",
            Profile::Audio => "audio",
            Profile::Text => "text",
            Profile::Pdf => "pdf",
            Profile::Archive => "archive",
        }
    }

    pub fn targets(self) -> &'static [Format] {
        match self {
            Profile::Image => IMAGE_TARGETS,
            Profile::Video => VIDEO_TARGETS,
            Profile::Audio => AUDIO_TARGETS,
            Profile::Text => TEXT_TARGETS,
            Profile::Pdf => PDF_TARGETS,
            Profile::Archive => ARCHIVE_TARGETS,
        }
    }
}

// -----------------------------------------------------------------------------
//  Quality presets and resolution
// -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Balanced,
    SmallerFile,
    HighQuality,
    Lossless,
}

pub const PRESETS: &[Preset] = &[
    Preset::Balanced,
    Preset::SmallerFile,
    Preset::HighQuality,
    Preset::Lossless,
];

impl Preset {
    pub fn label(self) -> &'static str {
        match self {
            Preset::Balanced => "Balanced",
            Preset::SmallerFile => "Smaller file",
            Preset::HighQuality => "High quality",
            Preset::Lossless => "Lossless",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Preset::Balanced => "Default sweet spot (CRF 22). Zero noticeable loss in visual quality for regular playback.",
            Preset::SmallerFile => "Aggressive compression for e-mail and web (CRF 28). Fine detail softens slightly.",
            Preset::HighQuality => "Near-transparent quality (CRF 18). Bigger files, keeps subtle gradients and grain.",
            Preset::Lossless => "Bit-exact output. Very large files; ideal for archiving masters.",
        }
    }

    pub fn index(self) -> usize {
        PRESETS.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Preset {
        *PRESETS.get(i).unwrap_or(&Preset::Balanced)
    }

    /// JPEG / WebP quality (1..=100).
    pub fn image_quality(self) -> u8 {
        match self {
            Preset::Balanced => 82,
            Preset::SmallerFile => 62,
            Preset::HighQuality => 93,
            Preset::Lossless => 100,
        }
    }

    /// AVIF constant quality (0..=100, higher = better).
    pub fn avif_quality(self) -> f32 {
        match self {
            Preset::Balanced => 62.0,
            Preset::SmallerFile => 45.0,
            Preset::HighQuality => 80.0,
            Preset::Lossless => 100.0,
        }
    }

    /// rav1e speed (0 slowest .. 10 fastest). Faster = bigger file at the same
    /// quality, so we trade a little size for conversion time.
    pub fn avif_speed(self) -> u8 {
        match self {
            Preset::Balanced => 6,
            Preset::SmallerFile => 8,
            Preset::HighQuality => 5,
            Preset::Lossless => 8,
        }
    }

    /// libx264 / libvpx-vp9 CRF.
    pub fn video_crf(self) -> &'static str {
        match self {
            Preset::Balanced => "22",
            Preset::SmallerFile => "28",
            Preset::HighQuality => "18",
            Preset::Lossless => "0",
        }
    }

    pub fn is_lossless(self) -> bool {
        self == Preset::Lossless
    }

    /// Audio bitrate for the lossy codecs.
    pub fn audio_bitrate(self) -> &'static str {
        match self {
            Preset::Balanced => "192k",
            Preset::SmallerFile => "128k",
            Preset::HighQuality => "256k",
            Preset::Lossless => "320k",
        }
    }

}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Original,
    P1080,
    P720,
    P480,
    Half,
}

pub const RESOLUTIONS: &[Resolution] = &[
    Resolution::Original,
    Resolution::P1080,
    Resolution::P720,
    Resolution::P480,
    Resolution::Half,
];

impl Resolution {
    pub fn label(self) -> &'static str {
        match self {
            Resolution::Original => "Original",
            Resolution::P1080 => "1080p",
            Resolution::P720 => "720p",
            Resolution::P480 => "480p",
            Resolution::Half => "Half size",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Resolution::Original => "Keep the source dimensions. No scaling, no resampling loss.",
            Resolution::P1080 => "Fit inside 1080 pixels tall, keeping the aspect ratio. Full-HD screens and phones.",
            Resolution::P720 => "Fit inside 720 pixels tall. Great for web video and e-mail.",
            Resolution::P480 => "Fit inside 480 pixels tall. Small previews and thumbnails.",
            Resolution::Half => "Halve both sides. A quarter of the pixels, roughly a quarter of the file size.",
        }
    }

    pub fn index(self) -> usize {
        RESOLUTIONS.iter().position(|r| *r == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Resolution {
        *RESOLUTIONS.get(i).unwrap_or(&Resolution::Original)
    }

    /// Height cap for video scaling.
    pub fn max_height(self) -> Option<u32> {
        match self {
            Resolution::Original | Resolution::Half => None,
            Resolution::P1080 => Some(1080),
            Resolution::P720 => Some(720),
            Resolution::P480 => Some(480),
        }
    }


    /// ffmpeg `scale` filter expression, or `None` when no scaling is wanted.
    pub fn ffmpeg_scale(self) -> Option<String> {
        match self {
            Resolution::Original => None,
            Resolution::Half => Some("scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string()),
            _ => {
                let h = self.max_height().unwrap_or(1080);
                // -2 keeps the width even, which every encoder requires.
                Some(format!("scale=-2:{h}"))
            }
        }
    }
}

// -----------------------------------------------------------------------------
//  Destination & collision policy (persisted in config.json)
// -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputStrategy {
    /// Write next to the source file.
    SameAsSource,
    /// Write into `custom_output_dir`.
    CustomDir,
}

impl Default for OutputStrategy {
    fn default() -> Self {
        OutputStrategy::SameAsSource
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollisionPolicy {
    /// Never touch an existing file: `name_1.ext`, `name_2.ext`, ...
    AutoRename,
    /// Replace the existing file atomically.
    Overwrite,
}

impl Default for CollisionPolicy {
    fn default() -> Self {
        CollisionPolicy::AutoRename
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_has_a_unique_label_and_extension() {
        for f in ALL {
            assert!(!f.label().is_empty());
            assert!(!f.help().is_empty(), "missing help for {f:?}");
            if let Some(ext) = f.extension() {
                assert!(!ext.starts_with('.'), "extension must not include a dot: {f:?}");
            }
        }
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.label(), b.label(), "duplicate label {a:?}");
            }
        }
    }

    #[test]
    fn profiles_are_consistent_with_categories() {
        assert!(Profile::Image.targets().contains(&Format::Pdf));
        assert!(Profile::Image.targets().len() <= 8, "UI shows targets in two rows of four");
        for p in [
            Profile::Image,
            Profile::Video,
            Profile::Audio,
            Profile::Text,
            Profile::Pdf,
            Profile::Archive,
        ] {
            assert!(p.targets().len() <= 8, "profile {} is too wide for the card", p.id());
            assert!(!p.targets().is_empty());
        }
    }

    #[test]
    fn preset_and_resolution_indices_round_trip() {
        for p in PRESETS {
            assert_eq!(Preset::from_index(p.index()), *p);
        }
        for r in RESOLUTIONS {
            assert_eq!(Resolution::from_index(r.index()), *r);
        }
    }

    #[test]
    fn scale_filters_are_even_dimensions() {
        assert_eq!(Resolution::Original.ffmpeg_scale(), None);
        assert_eq!(
            Resolution::P720.ffmpeg_scale().unwrap(),
            "scale=-2:720"
        );
    }
}
