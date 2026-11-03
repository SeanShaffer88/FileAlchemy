//! Portable/installed data-folder resolution.
//!
//! Spec §2: in portable mode *everything* must live inside `.\data\`. Nothing is
//! written to `%APPDATA%`, `%LOCALAPPDATA%`, `%PROGRAMDATA%` or `C:\Windows\Temp`
//! when a `data\` folder or a `portable.dat` sentinel sits next to the exe.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    /// Folder containing FileAlchemy.exe (may be a USB stick).
    pub exe_dir: PathBuf,
    /// Folder holding config.json and temp/.
    pub data_dir: PathBuf,
    pub config_file: PathBuf,
    pub temp_dir: PathBuf,
    pub portable: bool,
}

/// Resolve paths for the running executable.
pub fn resolve() -> Paths {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    resolve_for(&exe_dir, std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
}

/// Testable core: `local_appdata` is injected instead of read from the
/// environment so tests can run in parallel.
pub fn resolve_for(exe_dir: &Path, local_appdata: Option<PathBuf>) -> Paths {
    let data_next_to_exe = exe_dir.join("data");
    let sentinel = exe_dir.join("portable.dat");
    let portable = sentinel.is_file() || data_next_to_exe.is_dir();

    let data_dir = if portable {
        data_next_to_exe
    } else {
        match local_appdata {
            Some(base) => base.join("FileAlchemy"),
            // No LOCALAPPDATA (rare: service context). Stay inside the app
            // folder rather than guessing at a system-wide location.
            None => data_next_to_exe,
        }
    };

    Paths {
        exe_dir: exe_dir.to_path_buf(),
        config_file: data_dir.join("config.json"),
        temp_dir: data_dir.join("temp"),
        data_dir,
        portable,
    }
}

impl Paths {
    /// Create `data/` and `data/temp/` if they are missing.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.temp_dir)?;
        Ok(())
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.exe_dir.join("bin")
    }

    pub fn exe_path(&self) -> PathBuf {
        self.exe_dir.join("FileAlchemy.exe")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "filealchemy-paths-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn data_folder_next_to_exe_means_portable() {
        let exe_dir = scratch_dir("portable");
        std::fs::create_dir_all(exe_dir.join("data")).unwrap();

        let paths = resolve_for(&exe_dir, Some(PathBuf::from(r"C:\Users\me\AppData\Local")));
        assert!(paths.portable);
        assert_eq!(paths.data_dir, exe_dir.join("data"));
        assert_eq!(paths.temp_dir, exe_dir.join("data").join("temp"));
        assert_eq!(paths.config_file, exe_dir.join("data").join("config.json"));
        std::fs::remove_dir_all(&exe_dir).unwrap();
    }

    #[test]
    fn portable_sentinel_alone_enables_portable_mode() {
        let exe_dir = scratch_dir("sentinel");
        std::fs::write(exe_dir.join("portable.dat"), b"").unwrap();
        let paths = resolve_for(&exe_dir, Some(PathBuf::from(r"C:\Local")));
        assert!(paths.portable);
        std::fs::remove_dir_all(&exe_dir).unwrap();
    }

    #[test]
    fn installed_build_uses_local_appdata() {
        let exe_dir = scratch_dir("installed");
        let paths = resolve_for(&exe_dir, Some(PathBuf::from(r"C:\Users\me\AppData\Local")));
        assert!(!paths.portable);
        assert_eq!(
            paths.data_dir,
            PathBuf::from(r"C:\Users\me\AppData\Local").join("FileAlchemy")
        );
        std::fs::remove_dir_all(&exe_dir).unwrap();
    }

    #[test]
    fn everything_stays_inside_the_data_dir() {
        let exe_dir = scratch_dir("isolation");
        std::fs::create_dir_all(exe_dir.join("data")).unwrap();
        let paths = resolve_for(&exe_dir, Some(PathBuf::from(r"C:\Local")));
        assert!(paths.config_file.starts_with(&paths.data_dir));
        assert!(paths.temp_dir.starts_with(&paths.data_dir));
        std::fs::remove_dir_all(&exe_dir).unwrap();
    }
}
