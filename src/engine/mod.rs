//! Conversion engine.
//!
//! Layout
//! ------
//! * [`formats`]  – the format matrix shared with the UI.
//! * [`mime`]     – real content detection (magic bytes, containers, text).
//! * [`scratch`]  – scratch buffer + atomic placement of finished files.
//! * [`image_convert`] / [`pdf_writer`] – in-process image pipeline.
//! * [`archive`]  – in-process archive read/write/extract.
//! * [`media`]    – ffmpeg.exe argument construction and progress parsing.
//! * [`docs`]     – pandoc.exe / pdftoppm.exe plus the small native
//!                  CSV / JSON / RTF converters.
//! * [`external`] – child-process runner (JobObject-contained, cancellable).
//! * [`pipeline`] – worker pool, task registry, cancellation, events.
//!
//! Everything here is UI-agnostic so it can be unit tested and, in principle,
//! driven head-less.

pub mod archive;
pub mod docs;
pub mod external;
pub mod formats;
pub mod image_convert;
pub mod media;
pub mod mime;
pub mod pdf_writer;
pub mod pipeline;
pub mod scratch;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use formats::{CollisionPolicy, Format, Preset, Resolution};
use mime::Detected;

/// Cooperative cancellation flag. One per task; `true` means "stop now".
pub type CancelToken = Arc<AtomicBool>;

pub fn new_cancel_token() -> CancelToken {
    Arc::new(AtomicBool::new(false))
}

#[inline]
pub fn is_cancelled(token: &CancelToken) -> bool {
    token.load(Ordering::Relaxed)
}

#[derive(Debug)]
pub enum EngineError {
    /// The user pressed Esc (or removed the item) while we were working.
    Cancelled,
    Io(std::io::Error),
    /// The requested conversion is not in the format matrix.
    Unsupported { what: String, detail: String },
    /// A bundled tool is missing from `bin/`.
    ToolMissing { tool: &'static str, path: PathBuf },
    /// A bundled tool ran but reported failure.
    Tool {
        tool: &'static str,
        code: Option<i32>,
        message: String,
    },
    Decode(String),
    Encode(String),
    Internal(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Cancelled => write!(f, "Cancelled"),
            EngineError::Io(e) => write!(f, "{e}"),
            EngineError::Unsupported { what, detail } => write!(f, "{what}: {detail}"),
            EngineError::ToolMissing { tool, path } => write!(
                f,
                "{tool} is missing. FileAlchemy expects it at \"{}\". Reinstall FileAlchemy or restore the bin folder.",
                path.display()
            ),
            EngineError::Tool { tool, code, message } => {
                let trimmed = message.trim();
                let tail: String = trimmed
                    .lines()
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" ");
                match code {
                    Some(c) => write!(f, "{tool} failed (exit {c}): {tail}"),
                    None => write!(f, "{tool} failed: {tail}"),
                }
            }
            EngineError::Decode(m) => write!(f, "Could not read the source file: {m}"),
            EngineError::Encode(m) => write!(f, "Could not write the converted file: {m}"),
            EngineError::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// One conversion request, fully resolved by the UI thread so the worker does
/// not need to know anything about settings or window state.
#[derive(Clone, Debug)]
pub struct TaskRequest {
    pub id: u64,
    pub source: PathBuf,
    pub detected: Detected,
    pub target: Format,
    pub preset: Preset,
    pub resolution: Resolution,
    /// Directory the finished file(s) go into (already resolved).
    pub output_dir: PathBuf,
    pub collision: CollisionPolicy,
    /// Delete partial artefacts the moment a cancel is observed.
    pub purge_on_cancel: bool,
}

impl TaskRequest {
    pub fn stem(&self) -> String {
        self.source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "converted".to_string())
    }

    pub fn file_name(&self) -> String {
        self.source
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Output name for single-file targets, e.g. `holiday.webp` or, for the
    /// extract-to-folder target, the folder name. (Placement itself goes
    /// through `scratch::place_file`, which applies the collision policy.)
    #[allow(dead_code)]
    pub fn output_name(&self) -> String {
        match self.target.extension() {
            Some(ext) => format!("{}.{}", self.stem(), ext),
            None => self.stem(),
        }
    }
}

/// Long-lived engine state shared by all workers.
pub struct EngineContext {
    /// Private scratch area inside `data/temp`.
    pub scratch: Arc<scratch::Scratch>,
    /// Resolved paths to the bundled CLI engines.
    pub tools: external::ToolPaths,
}

/// Progress callback: `Some(0.0..=1.0)` for measurable work, `None` when the
/// engine cannot know (pandoc, pdftoppm) — the UI shows an indeterminate bar.
pub type ProgressFn<'a> = &'a mut dyn FnMut(Option<f32>);

/// Route one conversion to the right engine. Returns the files (or directory)
/// that were produced *inside the scratch area*; the caller is responsible for
/// moving them into place.
pub fn convert(
    req: &TaskRequest,
    ctx: &EngineContext,
    token: &CancelToken,
    progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }

    // Guard rail: the UI is built from the same table, so this only trips if a
    // malformed config or a command-line invocation asked for something odd.
    if !req.detected.targets().contains(&req.target) {
        return Err(EngineError::Unsupported {
            what: req.detected.kind.label().to_string(),
            detail: format!(
                "cannot become {}. Supported targets: {}.",
                req.target.label(),
                req.detected
                    .targets()
                    .iter()
                    .map(|f| f.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }

    if req.target.needs_ffmpeg() {
        ctx.tools.require_ffmpeg()?;
    }
    if req.target.needs_pandoc() {
        ctx.tools.require_pandoc()?;
    }
    if req.detected.kind == mime::InputKind::Pdf && req.target == Format::Png {
        ctx.tools.require_pdftoppm()?;
    }

    use mime::InputKind as K;
    match req.detected.kind {
        K::Png | K::Jpeg | K::Webp | K::Bmp | K::Tiff | K::Gif | K::Ico | K::Svg | K::Avif
        | K::Qoi => image_convert::run(req, ctx, token, progress),
        K::Mp4 | K::Mkv | K::Mov | K::Avi | K::Webm | K::Mp3 | K::Wav | K::Flac | K::Aac
        | K::Ogg | K::M4a => media::run(req, ctx, token, progress),
        K::Zip | K::SevenZ | K::Tar | K::Gzip | K::TarGz => {
            archive::run(req, ctx, token, progress)
        }
        K::Pdf | K::Docx | K::Epub | K::Markdown | K::Txt | K::Html | K::Rtf | K::Csv
        | K::Json => docs::run(req, ctx, token, progress),
    }
}

/// Human-readable size, used by the UI and by log lines.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// File size, or 0 when the file vanished.
pub fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_is_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MB");
        assert_eq!(human_size(1024 * 1024 * 250), "250 MB");
    }

    #[test]
    fn output_name_uses_target_extension() {
        let req = TaskRequest {
            id: 1,
            source: PathBuf::from(r"C:\photos\sunset.jpeg"),
            detected: mime::Detected {
                kind: mime::InputKind::Jpeg,
                mime: "image/jpeg".into(),
                confidence: mime::Confidence::Content,
                note: None,
            },
            target: Format::Webp,
            preset: Preset::Balanced,
            resolution: Resolution::Original,
            output_dir: PathBuf::from(r"C:\photos"),
            collision: CollisionPolicy::AutoRename,
            purge_on_cancel: true,
        };
        assert_eq!(req.output_name(), "sunset.webp");
        assert_eq!(req.stem(), "sunset");
    }
}
