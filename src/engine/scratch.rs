//! Scratch buffer and atomic file placement.
//!
//! Rules enforced here (see spec §4 "Atomic File Placement" and "Instant
//! Cancellation Scrubbing"):
//!
//! 1. Nothing is ever written directly to the destination. Converters write
//!    into `data/temp/task-<id>-<seq>/`, the file is flushed, then moved into
//!    place. A half-written file can therefore never be mistaken for a
//!    finished one.
//! 2. Placement is atomic wherever the OS allows it: `ReplaceFileW` when the
//!    target exists and we are allowed to overwrite, `MoveFileEx`
//!    (`std::fs::rename`) otherwise. Across volumes we stage a sibling
//!    `*.partial` file in the destination folder and rename it, so the visible
//!    name only ever appears complete.
//! 3. Every task directory is removed when the task ends — success, failure or
//!    cancellation — so `data/temp` never accumulates `.tmp` litter.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::formats::CollisionPolicy;
use super::{EngineError, Result};

/// Owner of the private scratch area (`data/temp`).
pub struct Scratch {
    root: PathBuf,
    seq: AtomicU64,
}

impl Scratch {
    /// Create (or adopt) the scratch root. Stale directories from a previous
    /// crash are purged: the single-instance mutex guarantees no other copy of
    /// FileAlchemy is using them.
    pub fn open(root: &Path) -> Result<Scratch> {
        fs::create_dir_all(root)?;
        let scratch = Scratch {
            root: root.to_path_buf(),
            seq: AtomicU64::new(0),
        };
        scratch.purge_all()?;
        Ok(scratch)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A fresh, empty directory for one task. Removed by the pipeline when the
    /// task finishes, and again by [`Scratch::purge_all`] at shutdown.
    pub fn task_dir(&self, task_id: u64) -> Result<PathBuf> {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let dir = self.root.join(format!("task-{task_id}-{n}"));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Delete the scratch directory of a finished task. Best effort: a file
    /// still held open by a dying child process must not turn into an error
    /// the user sees, because it will be swept on exit anyway.
    pub fn release_dir(&self, dir: &Path) {
        let _ = remove_dir_all_retrying(dir);
    }

    /// Remove every scratch directory belonging to `task_id`.
    pub fn release_task(&self, task_id: u64) {
        let prefix = format!("task-{task_id}-");
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = remove_dir_all_retrying(&entry.path());
            }
        }
    }

    /// Wipe everything inside the scratch root (called on startup and exit).
    pub fn purge_all(&self) -> io::Result<()> {
        let entries = match fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let _ = remove_dir_all_retrying(&path);
            } else {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = self.purge_all();
    }
}

fn remove_dir_all_retrying(dir: &Path) -> io::Result<()> {
    // A just-killed ffmpeg can hold its output handle for a few milliseconds.
    let mut last = Ok(());
    for attempt in 0..6 {
        match fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last = Err(e);
                std::thread::sleep(std::time::Duration::from_millis(40 * (attempt + 1)));
            }
        }
    }
    last
}

// -----------------------------------------------------------------------------
//  Placement
// -----------------------------------------------------------------------------

/// `dir/stem.ext`, or `dir/stem_1.ext`, `dir/stem_2.ext`, ... until free.
pub fn unique_path(dir: &Path, stem: &str, ext: Option<&str>) -> PathBuf {
    let build = |suffix: Option<u32>| -> PathBuf {
        let name = match suffix {
            None => stem.to_string(),
            Some(n) => format!("{stem}_{n}"),
        };
        match ext {
            Some(e) => dir.join(format!("{name}.{e}")),
            None => dir.join(name),
        }
    };
    let first = build(None);
    if !first.exists() {
        return first;
    }
    for n in 1..=10_000u32 {
        let candidate = build(Some(n));
        if !candidate.exists() {
            return candidate;
        }
    }
    build(Some(10_001))
}

/// Move `temp` into `dir` as `stem[.ext]`, honouring the collision policy.
/// Returns the final path.
pub fn place_file(
    temp: &Path,
    dir: &Path,
    stem: &str,
    ext: Option<&str>,
    policy: CollisionPolicy,
) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let target = match policy {
        CollisionPolicy::AutoRename => unique_path(dir, stem, ext),
        CollisionPolicy::Overwrite => match ext {
            Some(e) => dir.join(format!("{stem}.{e}")),
            None => dir.join(stem),
        },
    };

    if policy == CollisionPolicy::Overwrite {
        overwrite_atomic(temp, &target)?;
    } else {
        move_path(temp, &target)?;
    }
    Ok(target)
}

/// Move a staged directory into place (`Extract to folder`).
pub fn place_dir(temp: &Path, dir: &Path, stem: &str, policy: CollisionPolicy) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let target = match policy {
        CollisionPolicy::AutoRename => unique_path(dir, stem, None),
        CollisionPolicy::Overwrite => dir.join(stem),
    };
    if policy == CollisionPolicy::Overwrite && target.exists() {
        remove_dir_all_retrying(&target)?;
    }
    move_path(temp, &target)?;
    Ok(target)
}

/// Move a file or directory, falling back to copy+delete across volumes.
pub fn move_path(from: &Path, to: &Path) -> Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_across_volumes(from, to)?;
            Ok(())
        }
        Err(e) => Err(EngineError::Io(io::Error::new(
            e.kind(),
            format!("could not move result into place ({}): {e}", to.display()),
        ))),
    }
}

fn is_cross_device(e: &io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE (17) on Windows, EXDEV (18) on Unix.
    matches!(e.raw_os_error(), Some(17) | Some(18))
}

fn copy_across_volumes(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        copy_dir_recursive(from, to)?;
        remove_dir_all_retrying(from)?;
        return Ok(());
    }
    let staged = sibling_partial(to);
    {
        let mut src = File::open(from)?;
        let mut dst = File::create(&staged)?;
        io::copy(&mut src, &mut dst)?;
        // Make sure the bytes are on the platter before the rename.
        dst.sync_all()?;
    }
    match fs::rename(&staged, to) {
        Ok(()) => {}
        Err(_) => {
            // Last resort: identical-directory copy then replace.
            let _ = fs::remove_file(&staged);
            fs::copy(from, to)?;
        }
    }
    let _ = fs::remove_file(from);
    Ok(())
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn sibling_partial(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    target.with_file_name(format!(".{name}.partial"))
}

/// Replace `target` with `temp` atomically, keeping the target's ACLs where the
/// OS supports it (`ReplaceFileW`).
pub fn overwrite_atomic(temp: &Path, target: &Path) -> Result<()> {
    if !target.exists() {
        return move_path(temp, target);
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

        let replaced = crate::platform::wide(target.as_os_str());
        let replacement = crate::platform::wide(temp.as_os_str());
        let ok = unsafe {
            ReplaceFileW(
                replaced.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        if ok != 0 {
            return Ok(());
        }
        // ReplaceFileW refuses across volumes and for some exotic targets;
        // fall through to the portable path instead of failing the task.
    }

    // `std::fs::rename` maps to MoveFileEx with MOVEFILE_REPLACE_EXISTING on
    // Windows, which is atomic within a volume.
    match fs::rename(temp, target) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_across_volumes(temp, target)?;
            Ok(())
        }
        Err(e) => Err(EngineError::Io(io::Error::new(
            e.kind(),
            format!("could not replace {}: {e}", target.display()),
        ))),
    }
}

/// Turn an archive entry name into a safe relative path, or `None` when the
/// entry tries to escape the extraction root (`../`, absolute paths, drive
/// letters, NTFS alternate data streams).
pub fn sanitize_archive_entry(name: &str) -> Option<PathBuf> {
    // Archives always store '/' separators, whatever the platform.
    let normalised = name.replace('\\', "/");
    let mut out = PathBuf::new();
    for part in normalised.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            other if other.contains(':') => return None,
            other if other.ends_with(' ') || other.ends_with('.') => {
                // Windows silently strips these, which can turn "a. ." into
                // "a" and collide with another entry. Reject rather than guess.
                return None;
            }
            other => out.push(other),
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "filealchemy-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unique_path_appends_numeric_suffixes() {
        let root = tmp_root("unique");
        let a = unique_path(&root, "photo", Some("webp"));
        assert_eq!(a.file_name().unwrap(), "photo.webp");
        fs::write(&a, b"x").unwrap();

        let b = unique_path(&root, "photo", Some("webp"));
        assert_eq!(b.file_name().unwrap(), "photo_1.webp");
        fs::write(&b, b"x").unwrap();

        let c = unique_path(&root, "photo", Some("webp"));
        assert_eq!(c.file_name().unwrap(), "photo_2.webp");

        let d = unique_path(&root, "holiday", None);
        assert_eq!(d.file_name().unwrap(), "holiday");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn place_file_auto_rename_keeps_both_files() {
        let root = tmp_root("place");
        let out = root.join("out");
        let temp = root.join("work.tmp");
        fs::write(&temp, b"new").unwrap();
        let placed = place_file(&temp, &out, "doc", Some("pdf"), CollisionPolicy::AutoRename).unwrap();
        assert_eq!(placed.file_name().unwrap(), "doc.pdf");

        let temp2 = root.join("work2.tmp");
        fs::write(&temp2, b"second").unwrap();
        let placed2 =
            place_file(&temp2, &out, "doc", Some("pdf"), CollisionPolicy::AutoRename).unwrap();
        assert_eq!(placed2.file_name().unwrap(), "doc_1.pdf");
        assert_eq!(fs::read_to_string(&placed).unwrap(), "new");
        assert_eq!(fs::read_to_string(&placed2).unwrap(), "second");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn place_file_overwrite_replaces_content() {
        let root = tmp_root("overwrite");
        let out = root.join("out");
        fs::create_dir_all(&out).unwrap();
        let target = out.join("doc.txt");
        fs::write(&target, b"old").unwrap();

        let temp = root.join("new.tmp");
        fs::write(&temp, b"fresh").unwrap();
        let placed =
            place_file(&temp, &out, "doc", Some("txt"), CollisionPolicy::Overwrite).unwrap();
        assert_eq!(placed, target);
        assert_eq!(fs::read_to_string(&target).unwrap(), "fresh");
        assert!(!temp.exists(), "the scratch file must be consumed");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scratch_dirs_are_created_and_purged() {
        let root = tmp_root("scratch");
        let scratch = Scratch::open(&root).unwrap();
        let dir = scratch.task_dir(7).unwrap();
        assert!(dir.is_dir());
        fs::write(dir.join("a.tmp"), b"x").unwrap();
        scratch.release_dir(&dir);
        assert!(!dir.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn archive_entries_cannot_escape() {
        assert_eq!(
            sanitize_archive_entry("folder/file.txt").unwrap(),
            PathBuf::from("folder").join("file.txt")
        );
        assert!(sanitize_archive_entry("../evil.txt").is_none());
        assert!(sanitize_archive_entry("a/../../evil.txt").is_none());
        assert!(sanitize_archive_entry("C:/Windows/evil.txt").is_none());
        assert!(sanitize_archive_entry("stream.txt:hidden").is_none());
        // A leading slash is stripped, so the entry stays inside the root.
        assert_eq!(
            sanitize_archive_entry("/etc/passwd").unwrap(),
            PathBuf::from("etc").join("passwd")
        );
        assert!(sanitize_archive_entry("").is_none());
    }
}
