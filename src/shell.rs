//! Explorer context-menu integration (spec §5).
//!
//! Everything happens under `HKCU\Software\Classes\*\shell\FileAlchemy`, which
//! is per-user and therefore needs **no UAC prompt**. There is deliberately no
//! HKLM path and no `runas` verb anywhere in this file.
//!
//! Registry shape we create:
//!
//! ```text
//! HKCU\Software\Classes\*\shell\FileAlchemy
//!     (Default)          = "Convert with FileAlchemy"
//!     Icon               = "<exe>,0"
//!     MultiSelectModel   = "Player"      ; one invocation for N selected files
//!     command
//!         (Default)      = "\"<exe>\" \"%1\""
//! ```
//!
//! `MultiSelectModel = Player` is what lets a user select 20 files, right-click
//! once, and have all 20 enqueued in a single launch.

use std::path::Path;

pub const MENU_KEY: &str = r"Software\Classes\*\shell\FileAlchemy";
pub const MENU_LABEL: &str = "Convert with FileAlchemy";

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
    };

    use crate::platform::wide_str;
    use crate::shell::{MENU_KEY, MENU_LABEL};

    fn set_string(key: HKEY, name: Option<&str>, value: &str) -> u32 {
        let name_wide = match name {
            Some(n) => wide_str(n),
            None => vec![0],
        };
        let mut data: Vec<u16> = value.encode_utf16().collect();
        data.push(0); // REG_SZ values are NUL-terminated; cbData includes it.

        unsafe {
            RegSetValueExW(
                key,
                name_wide.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr() as *const u8,
                (data.len() * std::mem::size_of::<u16>()) as u32,
            )
        }
    }

    /// Register the per-user context menu for `exe`.
    pub fn install(exe: &Path) -> Result<(), String> {
        let exe = exe.to_path_buf();
        let key_path = wide_str(MENU_KEY);

        let mut key: HKEY = std::ptr::null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                key_path.as_ptr(),
                0,
                std::ptr::null_mut(),
                0, // REG_OPTION_NON_VOLATILE
                KEY_WRITE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!(
                "could not open HKCU\\{MENU_KEY} for writing (error {status}). \
                 Is the registry locked down by group policy?"
            ));
        }

        let exe_display = exe.display().to_string();
        let icon = format!("\"{exe_display}\",0");
        let command = format!("\"{exe_display}\" \"%1\"");

        let mut failure = None;
        for (name, value) in [
            (None, MENU_LABEL),
            (Some("Icon"), icon.as_str()),
            (Some("MultiSelectModel"), "Player"),
        ] {
            let status = set_string(key, name, value);
            if status != ERROR_SUCCESS {
                failure = Some(format!(
                    "could not write \"{}\" (error {status})",
                    name.unwrap_or("(Default)")
                ));
                break;
            }
        }

        if failure.is_none() {
            let command_path = wide_str("command");
            let mut command_key: HKEY = std::ptr::null_mut();
            let status = unsafe {
                RegCreateKeyExW(
                    key,
                    command_path.as_ptr(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    KEY_WRITE,
                    std::ptr::null(),
                    &mut command_key,
                    std::ptr::null_mut(),
                )
            };
            if status != ERROR_SUCCESS {
                failure = Some(format!("could not create the command subkey (error {status})"));
            } else {
                let status = set_string(command_key, None, &command);
                if status != ERROR_SUCCESS {
                    failure = Some(format!("could not write the command (error {status})"));
                }
                unsafe {
                    RegCloseKey(command_key);
                }
            }
        }

        unsafe {
            RegCloseKey(key);
        }
        match failure {
            Some(message) => Err(message),
            None => Ok(()),
        }
    }

    /// Remove the context menu. Idempotent: a missing key is success.
    pub fn uninstall() -> Result<(), String> {
        let key_path = wide_str(MENU_KEY);
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, key_path.as_ptr()) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(format!(
                "could not remove HKCU\\{MENU_KEY} (error {status})"
            ))
        }
    }

    /// Cheap check used to keep the settings toggle in sync.
    pub fn is_installed() -> bool {
        let key_path = wide_str(MENU_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                key_path.as_ptr(),
                0,
                KEY_READ,
                &mut key,
            )
        };
        if status == ERROR_SUCCESS {
            unsafe {
                RegCloseKey(key);
            }
            true
        } else {
            false
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub fn install(_exe: &Path) -> Result<(), String> {
        Err("Explorer integration is only available on Windows".into())
    }

    pub fn uninstall() -> Result<(), String> {
        Err("Explorer integration is only available on Windows".into())
    }

    pub fn is_installed() -> bool {
        false
    }
}

pub use imp::{install, is_installed, uninstall};

/// What the settings toggle should do, with a human-readable outcome.
pub enum ShellMenuChange {
    Installed,
    Removed,
    Failed(String),
}

pub fn set_enabled(enabled: bool, exe: &Path) -> ShellMenuChange {
    if enabled {
        match install(exe) {
            Ok(()) => ShellMenuChange::Installed,
            Err(e) => ShellMenuChange::Failed(e),
        }
    } else {
        match uninstall() {
            Ok(()) => ShellMenuChange::Removed,
            Err(e) => ShellMenuChange::Failed(e),
        }
    }
}
