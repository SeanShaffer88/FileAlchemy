//! In-process archive handling: ZIP, 7Z, TAR, GZ, TAR.GZ -> ZIP / 7Z / TAR.GZ /
//! extracted folder.
//!
//! Every archive is first unpacked into a private staging directory inside the
//! scratch area. That gives us three guarantees:
//!
//! * **No path traversal.** Entry names are sanitised (`../`, absolute paths,
//!   drive letters and NTFS streams are rejected or neutralised) *and* the tar
//!   reader applies its own `unpack_in` containment check.
//! * **All-or-nothing.** A corrupt archive fails before anything is written to
//!   the destination folder.
//! * **Uniform targets.** Packing always starts from a real directory, so
//!   converting ZIP -> 7Z -> TAR.GZ never accumulates container quirks.

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::engine::formats::Format;
use crate::engine::mime::InputKind;
use crate::engine::scratch::sanitize_archive_entry;
use crate::engine::{is_cancelled, CancelToken, EngineContext, EngineError, ProgressFn, Result, TaskRequest};

pub fn run(
    req: &TaskRequest,
    ctx: &EngineContext,
    token: &CancelToken,
    progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    let work = ctx.scratch.task_dir(req.id)?;
    let staging = work.join("extract");
    fs::create_dir_all(&staging)?;

    extract(req, &staging, token, progress)?;
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }

    let out = match req.target {
        Format::ExtractDir => {
            progress(Some(0.98));
            return Ok(vec![staging]);
        }
        Format::Zip => work.join("result.zip"),
        Format::SevenZ => work.join("result.7z"),
        Format::TarGz => work.join("result.tar.gz"),
        other => {
            return Err(EngineError::Unsupported {
                what: "archive".into(),
                detail: format!("{} is not an archive target", other.label()),
            })
        }
    };

    pack(req.target, &staging, &out, token, progress)?;
    Ok(vec![out])
}

// -----------------------------------------------------------------------------
//  Extraction
// -----------------------------------------------------------------------------

fn extract(
    req: &TaskRequest,
    dest: &Path,
    token: &CancelToken,
    progress: ProgressFn<'_>,
) -> Result<()> {
    match req.detected.kind {
        InputKind::Zip => extract_zip(&req.source, dest, token, progress),
        InputKind::SevenZ => extract_sevenz(&req.source, dest, token, progress),
        InputKind::Tar => extract_tar(File::open(&req.source)?, dest, token, progress),
        InputKind::TarGz => extract_tar(
            GzDecoder::new(File::open(&req.source)?),
            dest,
            token,
            progress,
        ),
        InputKind::Gzip => extract_single_gzip(req, dest, progress),
        other => Err(EngineError::Unsupported {
            what: other.label().into(),
            detail: "is not an archive".into(),
        }),
    }
}

fn extract_zip(
    source: &Path,
    dest: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<()> {
    let file = File::open(source)?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| EngineError::Decode(format!("ZIP: {e}")))?;
    let total = archive.len().max(1);

    for i in 0..archive.len() {
        if is_cancelled(token) {
            return Err(EngineError::Cancelled);
        }
        let mut entry = archive
            .by_index(i)
            .map_err(|e| EngineError::Decode(format!("ZIP entry {i}: {e}")))?;

        let raw_name = entry.name().to_string();
        let Some(relative) = sanitize_archive_entry(&raw_name) else {
            // Silently skip hostile entries instead of failing the whole job:
            // a single bad path in a 5 000 file archive should not block it.
            continue;
        };
        let target = dest.join(&relative);

        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = BufWriter::new(File::create(&target)?);
        std::io::copy(&mut entry, &mut out)?;
        out.flush()?;

        progress(Some(0.05 + 0.5 * (i as f32 / total as f32)));
    }
    Ok(())
}

fn extract_sevenz(
    source: &Path,
    dest: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<()> {
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }
    // sevenz-rust writes the entries itself; it rejects entries that would
    // escape the destination directory. Progress is indeterminate because the
    // crate does not expose a per-entry callback.
    progress(None);
    sevenz_rust::decompress_file(source, dest)
        .map_err(|e| EngineError::Decode(format!("7Z: {e}")))?;
    progress(Some(0.55));
    Ok(())
}

fn extract_tar<R: Read>(
    reader: R,
    dest: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);

    let entries = archive
        .entries()
        .map_err(|e| EngineError::Decode(format!("TAR: {e}")))?;
    let mut count = 0usize;
    for entry in entries {
        if is_cancelled(token) {
            return Err(EngineError::Cancelled);
        }
        let mut entry = entry.map_err(|e| EngineError::Decode(format!("TAR: {e}")))?;
        // `unpack_in` refuses absolute paths and `..` escapes by itself.
        match entry.unpack_in(dest) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Other => {
                // Unsafe entry (link outside the root): skip it.
                continue;
            }
            Err(e) => return Err(EngineError::Io(e)),
        }
        count += 1;
        if count % 16 == 0 {
            progress(None);
        }
    }
    progress(Some(0.55));
    Ok(())
}

fn extract_single_gzip(
    req: &TaskRequest,
    dest: &Path,
    mut progress: ProgressFn<'_>,
) -> Result<()> {
    let name = req
        .stem()
        .trim_end_matches(".tar")
        .trim_end_matches(".tgz")
        .to_string();
    let target = dest.join(if name.is_empty() { "content".to_string() } else { name });
    let mut decoder = GzDecoder::new(BufReader::new(File::open(&req.source)?));
    let mut out = BufWriter::new(File::create(&target)?);
    std::io::copy(&mut decoder, &mut out)?;
    out.flush()?;
    progress(Some(0.55));
    Ok(())
}

// -----------------------------------------------------------------------------
//  Packing
// -----------------------------------------------------------------------------

fn pack(
    target: Format,
    staging: &Path,
    out: &Path,
    token: &CancelToken,
    progress: ProgressFn<'_>,
) -> Result<()> {
    match target {
        Format::Zip => pack_zip(staging, out, token, progress),
        Format::SevenZ => pack_sevenz(staging, out, token, progress),
        Format::TarGz => pack_targz(staging, out, token, progress),
        other => Err(EngineError::Unsupported {
            what: "archive".into(),
            detail: format!("{} cannot be packed", other.label()),
        }),
    }
}

/// Depth-first list of `(relative name inside the archive, absolute path)`.
fn collect_files(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let meta = entry.metadata()?;
            if meta.is_dir() {
                walk(root, &path, out)?;
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, path));
            }
        }
        Ok(())
    }

    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

fn pack_zip(
    staging: &Path,
    out: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<()> {
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let files = collect_files(staging)?;
    let total = files.len().max(1);
    let file = File::create(out)?;
    let mut writer = ZipWriter::new(BufWriter::new(file));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    for (i, (name, path)) in files.iter().enumerate() {
        if is_cancelled(token) {
            return Err(EngineError::Cancelled);
        }
        writer
            .start_file(name.clone(), options)
            .map_err(|e| EngineError::Encode(format!("ZIP: {e}")))?;
        let mut input = File::open(path)?;
        std::io::copy(&mut input, &mut writer)?;
        progress(Some(0.55 + 0.4 * (i as f32 / total as f32)));
    }

    let mut inner = writer
        .finish()
        .map_err(|e| EngineError::Encode(format!("ZIP: {e}")))?;
    inner.flush()?;
    let file = inner
        .into_inner()
        .map_err(|e| EngineError::Encode(e.to_string()))?;
    file.sync_all()?;
    Ok(())
}

fn pack_sevenz(
    staging: &Path,
    out: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<()> {
    let files = collect_files(staging)?;
    let total = files.len().max(1);

    // NOTE: this is the only place that touches the sevenz-rust writer API; if
    // the crate's signatures move, this function is the single point of change.
    let mut writer = sevenz_rust::SevenZWriter::create(out)
        .map_err(|e| EngineError::Encode(format!("7Z: {e}")))?;

    for (i, (name, path)) in files.iter().enumerate() {
        if is_cancelled(token) {
            return Err(EngineError::Cancelled);
        }
        let input = File::open(path)?;
        writer
            .push_source(input, name, false)
            .map_err(|e| EngineError::Encode(format!("7Z: {e}")))?;
        progress(Some(0.55 + 0.4 * (i as f32 / total as f32)));
    }

    writer
        .finish()
        .map_err(|e| EngineError::Encode(format!("7Z: {e}")))?;
    Ok(())
}

fn pack_targz(
    staging: &Path,
    out: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<()> {
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }
    progress(Some(0.6));

    let file = File::create(out)?;
    let encoder = GzEncoder::new(BufWriter::new(file), Compression::default());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    builder
        .append_dir_all(".", staging)
        .map_err(|e| EngineError::Encode(format!("TAR: {e}")))?;
    progress(Some(0.9));
    builder
        .finish()
        .map_err(|e| EngineError::Encode(format!("TAR: {e}")))?;
    let mut encoder = builder
        .into_inner()
        .map_err(|e| EngineError::Encode(e.to_string()))?;
    encoder.flush()?;
    let file = encoder
        .finish()
        .map_err(|e| EngineError::Encode(e.to_string()))?
        .into_inner()
        .map_err(|e| EngineError::Encode(e.to_string()))?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("filealchemy-arc-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_tree(root: &Path) {
        fs::create_dir_all(root.join("images")).unwrap();
        fs::write(root.join("readme.txt"), b"hello").unwrap();
        fs::write(root.join("images/logo.svg"), b"<svg/>").unwrap();
    }

    fn no_op(_: Option<f32>) {}

    #[test]
    fn zip_roundtrip_preserves_tree() {
        let root = tmp_root("zip");
        let src = root.join("src");
        sample_tree(&src);
        let token = crate::engine::new_cancel_token();
        let mut progress = no_op;

        let archive = root.join("a.zip");
        pack_zip(&src, &archive, &token, &mut progress).unwrap();

        let dest = root.join("out");
        fs::create_dir_all(&dest).unwrap();
        extract_zip(&archive, &dest, &token, &mut progress).unwrap();

        assert_eq!(fs::read_to_string(dest.join("readme.txt")).unwrap(), "hello");
        assert!(dest.join("images/logo.svg").is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn targz_roundtrip_preserves_tree() {
        let root = tmp_root("targz");
        let src = root.join("src");
        sample_tree(&src);
        let token = crate::engine::new_cancel_token();
        let mut progress = no_op;

        let archive = root.join("a.tar.gz");
        pack_targz(&src, &archive, &token, &mut progress).unwrap();

        let dest = root.join("out");
        fs::create_dir_all(&dest).unwrap();
        extract_tar(
            GzDecoder::new(File::open(&archive).unwrap()),
            &dest,
            &token,
            &mut progress,
        )
        .unwrap();

        assert_eq!(fs::read_to_string(dest.join("readme.txt")).unwrap(), "hello");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hostile_zip_entries_stay_inside_the_target() {
        use zip::write::SimpleFileOptions;
        let root = tmp_root("zip-slip");
        let archive_path = root.join("evil.zip");
        {
            let mut writer = zip::ZipWriter::new(File::create(&archive_path).unwrap());
            writer.start_file("../escaped.txt", SimpleFileOptions::default()).unwrap();
            writer.write_all(b"nope").unwrap();
            writer.start_file("safe.txt", SimpleFileOptions::default()).unwrap();
            writer.write_all(b"ok").unwrap();
            writer.finish().unwrap();
        }

        let dest = root.join("out");
        fs::create_dir_all(&dest).unwrap();
        let token = crate::engine::new_cancel_token();
        let mut progress = no_op;
        extract_zip(&archive_path, &dest, &token, &mut progress).unwrap();

        assert!(!root.join("escaped.txt").exists(), "zip-slip was not blocked");
        assert_eq!(fs::read_to_string(dest.join("safe.txt")).unwrap(), "ok");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cancellation_is_honoured_before_work_starts() {
        let root = tmp_root("cancel");
        let archive = root.join("whatever.zip");
        fs::write(&archive, b"PK\x05\x06").unwrap();
        let token = crate::engine::new_cancel_token();
        token.store(true, std::sync::atomic::Ordering::Relaxed);
        let dest = root.join("out");
        let mut progress = no_op;
        let err = extract_zip(&archive, &dest, &token, &mut progress).unwrap_err();
        assert!(matches!(err, EngineError::Cancelled));
        fs::remove_dir_all(&root).unwrap();
    }
}
