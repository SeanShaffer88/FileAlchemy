//! Audio & video conversion through the bundled `ffmpeg.exe`.
//!
//! `ffmpeg.exe` is a ~90 MB static build shipped in `bin/`; nothing is
//! downloaded at runtime. We drive it with an explicit argument list (no shell,
//! no string interpolation of user data into a command line) and parse the
//! machine-readable `-progress` stream for real percentages.

use std::path::{Path, PathBuf};

use crate::engine::external::{self, CommandSpec, Stream, Tool};
use crate::engine::formats::{Format, Resolution};
use crate::engine::{is_cancelled, CancelToken, EngineContext, EngineError, ProgressFn, Result, TaskRequest};

pub fn run(
    req: &TaskRequest,
    ctx: &EngineContext,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    let ffmpeg = ctx.tools.require(Tool::Ffmpeg)?;
    let work = ctx.scratch.task_dir(req.id)?;
    let ext = req
        .target
        .extension()
        .ok_or_else(|| EngineError::Internal("media target without an extension".into()))?;
    let out_path = work.join(format!("result.{ext}"));

    progress(Some(0.01));
    let duration = probe_duration(&ffmpeg, &req.source);
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }

    let spec = build_command(&ffmpeg, req, &out_path);
    let mut last_reported = -1.0f32;

    let outcome = external::run_streaming(
        "ffmpeg",
        &spec,
        token,
        |stream, line| {
            if stream != Stream::Stdout {
                return;
            }
            let Some(seconds) = parse_progress_seconds(line) else {
                return;
            };
            if let Some(total) = duration {
                if total > 0.0 {
                    let fraction = (seconds / total).clamp(0.0, 0.995) as f32;
                    // Only wake the UI when the number actually moves a percent.
                    if fraction - last_reported >= 0.01 {
                        last_reported = fraction;
                        progress(Some(fraction));
                    }
                }
            }
        },
    )?;

    if outcome.code.unwrap_or(1) != 0 {
        return Err(EngineError::Tool {
            tool: "ffmpeg",
            code: outcome.code,
            message: summarise_stderr(&outcome.stderr),
        });
    }
    if !out_path.is_file() {
        return Err(EngineError::Tool {
            tool: "ffmpeg",
            code: outcome.code,
            message: "ffmpeg reported success but produced no output file".into(),
        });
    }

    progress(Some(0.99));
    Ok(vec![out_path])
}

pub fn build_command(ffmpeg: &Path, req: &TaskRequest, out_path: &Path) -> CommandSpec {
    let mut spec = CommandSpec::new(ffmpeg);
    spec = spec.args([
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-nostats",
        "-progress",
        "pipe:1",
        "-i",
    ]);
    spec.args.push(req.source.to_string_lossy().into_owned());

    let preset = req.preset;
    let bitrate = preset.audio_bitrate();

    match req.target {
        Format::Mp4 => {
            spec = spec.args(["-c:v", "libx264", "-preset", "medium", "-crf", preset.video_crf()]);
            if !preset.is_lossless() {
                spec = spec.args(["-pix_fmt", "yuv420p"]);
            }
            if let Some(scale) = scale_filter(req.resolution, false) {
                spec = spec.args(["-vf", scale.as_str()]);
            }
            spec = spec
                .args(["-c:a", "aac", "-b:a", bitrate])
                .args(["-movflags", "+faststart"]);
        }
        Format::Webm => {
            spec = spec
                .args(["-c:v", "libvpx-vp9", "-b:v", "0"])
                .args(["-row-mt", "1"]);
            if preset.is_lossless() {
                spec = spec.args(["-lossless", "1"]);
            } else {
                spec = spec.args(["-crf", preset.video_crf()]);
            }
            if let Some(scale) = scale_filter(req.resolution, false) {
                spec = spec.args(["-vf", scale.as_str()]);
            }
            spec = spec.args(["-c:a", "libopus", "-b:a", bitrate]);
        }
        Format::Gif => {
            // Two-pass palette in a single filter graph: without the palette
            // step, GIF dithering looks like 1996.
            let scale = scale_filter(req.resolution, true).unwrap_or_default();
            let graph = format!(
                "[0:v]fps=12,{scale}split[a][b];[a]palettegen=stats_mode=diff[p];\
                 [b][p]paletteuse=dither=bayer:bayer_scale=3[v]"
            );
            spec = spec
                .args(["-filter_complex", graph.as_str()])
                .args(["-map", "[v]"])
                .args(["-loop", "0"]);
        }
        Format::Mp3 => {
            spec = spec.args(["-vn", "-sn", "-dn", "-c:a", "libmp3lame", "-b:a", bitrate]);
        }
        Format::Wav => {
            spec = spec.args(["-vn", "-sn", "-dn", "-c:a", "pcm_s16le"]);
        }
        Format::Flac => {
            spec = spec.args(["-vn", "-sn", "-dn", "-c:a", "flac"]);
            if preset.is_lossless() {
                spec = spec.args(["-compression_level", "8"]);
            }
        }
        Format::M4a => {
            spec = spec
                .args(["-vn", "-sn", "-dn", "-c:a", "aac", "-b:a", bitrate])
                .args(["-movflags", "+faststart"]);
        }
        _ => {}
    }

    spec.args.push(out_path.to_string_lossy().into_owned());
    spec
}

/// `-vf` fragment. Returns `None` when no scaling is requested.
fn scale_filter(resolution: Resolution, comma_suffix: bool) -> Option<String> {
    let base = resolution.ffmpeg_scale()?;
    Some(if comma_suffix && resolution != Resolution::Original {
        format!("{base},")
    } else {
        base
    })
}

// -----------------------------------------------------------------------------
//  Progress parsing
// -----------------------------------------------------------------------------

/// `key=value` line from `-progress`. Returns elapsed seconds.
///
/// ffmpeg emits `out_time_us` (newer) and `out_time_ms` (older); despite the
/// name the latter is also *microseconds*, a long-standing ffmpeg quirk that we
/// compensate for by falling back to `out_time=HH:MM:SS.mmm` when the value
/// would make no sense.
pub fn parse_progress_seconds(line: &str) -> Option<f64> {
    let (key, value) = line.split_once('=')?;
    match key.trim() {
        "out_time_us" | "out_time_ms" => {
            let micros: i64 = value.trim().parse().ok()?;
            if micros < 0 {
                return None;
            }
            Some(micros as f64 / 1_000_000.0)
        }
        "out_time" => parse_timestamp(value.trim()),
        _ => None,
    }
}

/// `HH:MM:SS.mmm`, `MM:SS.mmm` or `SS.mmm` -> seconds.
pub fn parse_timestamp(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut total = 0.0f64;
    let mut scale = 1.0f64;
    let mut fields = 0;
    for part in text.split(':').rev() {
        let value: f64 = part.trim().parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        total += value * scale;
        scale *= 60.0;
        fields += 1;
        if fields > 3 {
            return None;
        }
    }
    if fields == 0 {
        None
    } else {
        Some(total)
    }
}

/// Ask ffmpeg how long the input is. Runs `ffmpeg -i file`, which always exits
/// non-zero (no output specified) but prints the container metadata first.
pub fn probe_duration(ffmpeg: &Path, input: &Path) -> Option<f64> {
    let spec = CommandSpec::new(ffmpeg)
        .args(["-hide_banner", "-i"])
        .arg(input.to_string_lossy().into_owned());
    let text = external::capture(&spec).ok()?;
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with("Duration:"))?;
    let value = line.split("Duration:").nth(1)?.trim();
    let timestamp = value.split(',').next()?.trim();
    parse_timestamp(timestamp)
}

/// Keep the meaningful part of ffmpeg's error output.
fn summarise_stderr(stderr: &str) -> String {
    let mut lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with("frame=") && !l.starts_with("size="))
        .collect();
    if lines.is_empty() {
        return "ffmpeg failed without an explanation".into();
    }
    lines.truncate(6);
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::formats::{CollisionPolicy, Preset};
    use crate::engine::mime::{Confidence, Detected, InputKind};

    fn request(target: Format, preset: Preset, resolution: Resolution) -> TaskRequest {
        TaskRequest {
            id: 1,
            source: PathBuf::from(r"C:\videos\clip.mov"),
            detected: Detected {
                kind: InputKind::Mov,
                mime: "video/quicktime".into(),
                confidence: Confidence::Content,
                note: None,
            },
            target,
            preset,
            resolution,
            output_dir: PathBuf::from(r"C:\out"),
            collision: CollisionPolicy::AutoRename,
            purge_on_cancel: true,
        }
    }

    #[test]
    fn mp4_arguments_are_explicit_and_complete() {
        let req = request(Format::Mp4, Preset::Balanced, Resolution::P720);
        let spec = build_command(Path::new("ffmpeg.exe"), &req, Path::new("out.mp4"));
        let line = spec.args.join(" ");
        assert!(line.contains("-c:v libx264"));
        assert!(line.contains("-crf 22"));
        assert!(line.contains("-c:a aac"));
        assert!(line.contains("-b:a 192k"));
        assert!(line.contains("-vf scale=-2:720"));
        assert!(line.contains("-movflags +faststart"));
        assert_eq!(spec.args.last().unwrap(), "out.mp4");
    }

    #[test]
    fn mp3_extraction_drops_video_streams() {
        let req = request(Format::Mp3, Preset::SmallerFile, Resolution::Original);
        let spec = build_command(Path::new("ffmpeg.exe"), &req, Path::new("out.mp3"));
        let line = spec.args.join(" ");
        assert!(line.contains("-vn"));
        assert!(line.contains("-b:a 128k"));
        assert!(!line.contains("-vf"));
    }

    #[test]
    fn lossless_video_uses_crf_zero() {
        let req = request(Format::Mp4, Preset::Lossless, Resolution::Original);
        let spec = build_command(Path::new("ffmpeg.exe"), &req, Path::new("out.mp4"));
        assert!(spec.args.join(" ").contains("-crf 0"));
    }

    #[test]
    fn gif_uses_a_two_pass_palette_graph() {
        let req = request(Format::Gif, Preset::Balanced, Resolution::P480);
        let spec = build_command(Path::new("ffmpeg.exe"), &req, Path::new("out.gif"));
        let line = spec.args.join(" ");
        assert!(line.contains("palettegen"), "{line}");
        assert!(line.contains("paletteuse"), "{line}");
        assert!(line.contains("scale=-2:480"), "{line}");
        assert!(line.contains("-map [v]"), "{line}");
    }

    #[test]
    fn progress_lines_are_understood() {
        assert_eq!(parse_progress_seconds("out_time_us=1500000"), Some(1.5));
        assert_eq!(parse_progress_seconds("out_time_ms=2500000"), Some(2.5));
        assert_eq!(parse_progress_seconds("out_time=00:00:03.50"), Some(3.5));
        assert_eq!(parse_progress_seconds("progress=continue"), None);
        assert_eq!(parse_progress_seconds("garbage"), None);
    }

    #[test]
    fn timestamps_parse() {
        assert_eq!(parse_timestamp("00:00:12.34"), Some(12.34));
        assert_eq!(parse_timestamp("01:02:03.00"), Some(3723.0));
        assert_eq!(parse_timestamp("nonsense"), None);
    }
}
