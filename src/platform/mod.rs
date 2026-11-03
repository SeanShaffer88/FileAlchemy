//! Platform layer.
//!
//! Everything Windows-specific lives behind this module so the conversion
//! engine and its tests stay portable. On non-Windows hosts a documented
//! fallback keeps the crate compiling (`cargo check`/`cargo test` on CI
//! containers), while the GUI itself is Windows-targeted.

use std::ffi::OsStr;

/// Window title. Also used by the second instance to find the first one.
pub const APP_TITLE: &str = "FileAlchemy";

/// Named mutex for single-instance enforcement (per logon session).
pub const MUTEX_NAME: &str = "Local\\FileAlchemy_SingleInstance_Mutex";

/// Header magic for `WM_COPYDATA` path forwarding.
pub const COPYDATA_MAGIC: usize = 0x4641_4C43; // 'FALC'

/// UTF-16, NUL-terminated, ready for a `PCWSTR`.
pub fn wide(os: &OsStr) -> Vec<u16> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        os.encode_wide().chain(std::iter::once(0)).collect()
    }
    #[cfg(not(windows))]
    {
        os.to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }
}

pub fn wide_str(s: &str) -> Vec<u16> {
    wide(OsStr::new(s))
}

/// UTF-16 buffer -> String, stopping at the first NUL.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(not(windows))]
mod fallback;
#[cfg(not(windows))]
pub use fallback::*;
