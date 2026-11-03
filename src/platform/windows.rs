//! Windows implementation of the platform layer.
//!
//! Contains, in order:
//! * `console`  – attach to the parent console only when launched from one;
//! * `jobobject`– process-wide JobObject with KILL_ON_JOB_CLOSE (spec §3);
//! * `single_instance` – named mutex + `WM_COPYDATA` path forwarding;
//! * `window`   – WndProc chaining for WM_DROPFILES / WM_COPYDATA / hotkeys,
//!                immersive dark mode, taskbar flash and completion chime;
//! * `dialogs`  – native Open / Browse-for-folder dialogs (comdlg32 + shell32).
//!
//! Design note: we chain the window procedure with `SetWindowLongPtrW` +
//! `CallWindowProcW` instead of using `SetWindowSubclass`. The Slint/winit
//! backend creates the HWND and never replaces its WndProc afterwards, and
//! plain chaining avoids a comctl32 dependency for a 20-line hook.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, EnumWindows, FindWindowW, FlashWindowEx,
    GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible, MessageBeep, SendMessageW,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, COPYDATASTRUCT, FLASHWINFO, FLASHW_ALL,
    FLASHW_TIMERNOFG, GWLP_WNDPROC, MB_ICONASTERISK, SW_RESTORE, WM_COPYDATA, WM_DROPFILES,
    WM_KEYDOWN, WM_SYSKEYDOWN, WNDPROC,
};

use crate::platform::{from_wide, wide_str, COPYDATA_MAGIC};

// =============================================================================
//  Console
// =============================================================================

pub mod console {
    use std::io::Write;
    use std::mem::ManuallyDrop;
    use std::os::windows::io::FromRawHandle;

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };

    /// A GUI subsystem binary that was started from cmd/PowerShell can borrow
    /// the parent console so `--install-shell-menu` and `--help` are visible.
    /// Launched from Explorer there is no parent console and this is a no-op.
    pub fn attach() -> bool {
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 }
    }

    /// Write one line to the parent console, if we are attached to one.
    pub fn note(message: &str) {
        for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            unsafe {
                let handle = GetStdHandle(std_handle);
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    continue;
                }
                let mut file = ManuallyDrop::new(std::fs::File::from_raw_handle(handle as _));
                let _ = file.write_all(message.as_bytes());
                let _ = file.write_all(b"\r\n");
                let _ = file.flush();
            }
        }
    }
}

// =============================================================================
//  JobObject containment
// =============================================================================

pub mod jobobject {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A Windows JobObject whose processes die with the job.
    pub struct ProcessJob(HANDLE);

    // HANDLEs are just kernel object references: safe to share.
    unsafe impl Send for ProcessJob {}
    unsafe impl Sync for ProcessJob {}

    impl ProcessJob {
        fn create() -> Option<ProcessJob> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }

                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    let _ = CloseHandle(job);
                    return None;
                }
                Some(ProcessJob(job))
            }
        }

        /// Put a freshly spawned child into the job. Failure is tolerated: the
        /// conversion still works, it just loses kernel-level containment.
        pub fn assign(&self, child: &Child) -> bool {
            unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) != 0 }
        }

        /// Kill every process in the job (used on shutdown).
        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for ProcessJob {
        fn drop(&mut self) {
            unsafe {
                // The handle closing is itself what kills the tree, thanks to
                // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.
                CloseHandle(self.0);
            }
        }
    }

    static JOB: OnceLock<Option<ProcessJob>> = OnceLock::new();

    pub fn global() -> Option<&'static ProcessJob> {
        JOB.get_or_init(ProcessJob::create).as_ref()
    }

    pub fn assign_to_global_job(child: &Child) {
        if let Some(job) = global() {
            job.assign(child);
        }
    }

    pub fn terminate_all() {
        if let Some(job) = global() {
            job.terminate();
        }
    }
}

// =============================================================================
//  Single instance
// =============================================================================

pub mod single_instance {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    use crate::platform::{wide_str, MUTEX_NAME};

    /// Owns the single-instance mutex for the lifetime of the process.
    pub struct SingleInstance {
        handle: HANDLE,
        already_running: bool,
    }

    impl SingleInstance {
        pub fn already_running(&self) -> bool {
            self.already_running
        }
    }

    /// `None` means the mutex could not be created at all (extremely rare); the
    /// app then continues as if it were the only instance.
    pub fn acquire() -> Option<SingleInstance> {
        let name = wide_str(MUTEX_NAME);
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return None;
            }
            let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
            if already_running {
                // We do not own it; close our handle and report the conflict.
                CloseHandle(handle);
                return Some(SingleInstance {
                    handle: std::ptr::null_mut(),
                    already_running: true,
                });
            }
            Some(SingleInstance {
                handle,
                already_running: false,
            })
        }
    }

    impl Drop for SingleInstance {
        fn drop(&mut self) {
            if !self.handle.is_null() {
                unsafe {
                    CloseHandle(self.handle);
                }
            }
        }
    }
}

// =============================================================================
//  Window hooks, notifications, dark mode
// =============================================================================

/// Commands raised from the native event loop and consumed by the UI timer.
#[derive(Clone, Debug)]
pub enum Command {
    /// Files arrived over WM_DROPFILES or WM_COPYDATA from a second instance.
    AddPaths(Vec<PathBuf>),
    ConvertAll,
    CancelAll,
    Browse,
    OpenSettings,
    /// The HWND exists now: dark mode, DnD and hooks are installed.
    WindowReady(isize),
    /// Restore + activate an existing window (second instance launch).
    BringToFront,
}

pub type CommandQueue = Arc<Mutex<VecDeque<Command>>>;

pub fn new_command_queue() -> CommandQueue {
    Arc::new(Mutex::new(VecDeque::new()))
}

pub fn push(queue: &CommandQueue, command: Command) {
    if let Ok(mut queue) = queue.lock() {
        queue.push_back(command);
    }
}

static COMMANDS: OnceLock<CommandQueue> = OnceLock::new();
static ORIGINAL_WNDPROC: AtomicIsize = AtomicIsize::new(0);
static OUR_HWND: AtomicIsize = AtomicIsize::new(0);
static ENUM_RESULT: AtomicIsize = AtomicIsize::new(0);

/// Find our own top-level window by enumerating windows owned by this process.
pub fn find_own_window(timeout_ms: u64) -> Option<isize> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        ENUM_RESULT.store(0, Ordering::SeqCst);
        unsafe {
            EnumWindows(Some(enum_own_window), 0);
        }
        let found = ENUM_RESULT.load(Ordering::SeqCst);
        if found != 0 {
            return Some(found);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

unsafe extern "system" fn enum_own_window(hwnd: HWND, _lparam: LPARAM) -> BOOL {
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == GetCurrentProcessId()
        && IsWindowVisible(hwnd) != 0
        && GetWindowTextLengthW(hwnd) > 0
    {
        ENUM_RESULT.store(hwnd as isize, Ordering::SeqCst);
        return 0; // stop enumerating
    }
    1
}

/// Find the window of an already-running instance (used by the 2nd launch).
pub fn find_existing_window() -> Option<isize> {
    let title = wide_str(crate::platform::APP_TITLE);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(3000);
    loop {
        let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
        if !hwnd.is_null() {
            return Some(hwnd as isize);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Install the WndProc hook, accept file drops, and apply dark mode.
/// Returns the HWND on success.
pub fn install(queue: CommandQueue) -> Option<isize> {
    let _ = COMMANDS.set(queue);
    let hwnd = find_own_window(5000)?;
    OUR_HWND.store(hwnd, Ordering::SeqCst);

    unsafe {
        let hook: WNDPROC = Some(wndproc);
        let new_proc = hook? as usize as isize;
        let previous = SetWindowLongPtrW(hwnd as HWND, GWLP_WNDPROC, new_proc);
        if previous == 0 {
            return None;
        }
        ORIGINAL_WNDPROC.store(previous, Ordering::SeqCst);
        DragAcceptFiles(hwnd as HWND, 1);
    }

    set_dark_mode(hwnd);
    Some(hwnd)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DROPFILES => {
            handle_drop(wparam);
            return 0;
        }
        WM_COPYDATA => {
            if handle_copy_data(lparam) {
                bring_to_front(hwnd as isize);
                return 1;
            }
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if handle_hotkey(wparam as u32) {
                return 0;
            }
        }
        _ => {}
    }

    let previous = ORIGINAL_WNDPROC.load(Ordering::SeqCst);
    if previous != 0 {
        let original: WNDPROC = std::mem::transmute(previous);
        return CallWindowProcW(original, hwnd, msg, wparam, lparam);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn handle_drop(hdrop: WPARAM) {
    let mut paths = Vec::new();
    unsafe {
        let count = DragQueryFileW(hdrop as _, u32::MAX, std::ptr::null_mut(), 0);
        for index in 0..count {
            let mut buf = [0u16; 4096];
            let len = DragQueryFileW(hdrop as _, index, buf.as_mut_ptr(), buf.len() as u32);
            if len > 0 {
                paths.push(PathBuf::from(from_wide(&buf[..len as usize])));
            }
        }
        DragFinish(hdrop as _);
    }
    if !paths.is_empty() {
        if let Some(queue) = COMMANDS.get() {
            push(queue, Command::AddPaths(paths));
        }
    }
}

fn handle_copy_data(lparam: LPARAM) -> bool {
    if lparam == 0 {
        return false;
    }
    unsafe {
        let payload = &*(lparam as *const COPYDATASTRUCT);
        if payload.dwData != COPYDATA_MAGIC || payload.cbData == 0 || payload.lpData.is_null() {
            return false;
        }
        let units = payload.cbData as usize / 2;
        let slice = std::slice::from_raw_parts(payload.lpData as *const u16, units);
        // Double-NUL separated list of absolute paths.
        let mut paths = Vec::new();
        let mut start = 0usize;
        for (i, unit) in slice.iter().enumerate() {
            if *unit == 0 {
                if i == start {
                    break;
                }
                paths.push(PathBuf::from(from_wide(&slice[start..i])));
                start = i + 1;
            }
        }
        if paths.is_empty() {
            return false;
        }
        if let Some(queue) = COMMANDS.get() {
            push(queue, Command::AddPaths(paths.clone()));
            push(queue, Command::BringToFront);
        }
        true
    }
}

/// Enter / Esc / Ctrl+O / Ctrl+, are handled here because they must work no
/// matter which widget currently has keyboard focus (or none at all).
fn handle_hotkey(vk: u32) -> bool {
    const VK_ESCAPE: u32 = 0x1B;
    const VK_RETURN: u32 = 0x0D;
    const VK_OEM_COMMA: u32 = 0xBC;

    let ctrl = unsafe { (GetKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0 };
    let Some(queue) = COMMANDS.get() else {
        return false;
    };

    let command = match (vk, ctrl) {
        (VK_ESCAPE, false) => Command::CancelAll,
        (VK_RETURN, false) => Command::ConvertAll,
        (0x4F, true) => Command::Browse,        // Ctrl+O
        (VK_OEM_COMMA, true) => Command::OpenSettings, // Ctrl+,
        _ => return false,
    };
    push(queue, command);
    true
}

/// Forward paths to an already-running instance. Returns `false` if the window
/// did not accept them (it will be relaunched instead).
pub fn send_paths(hwnd: isize, paths: &[PathBuf]) -> bool {
    if paths.is_empty() {
        return true;
    }
    let mut data: Vec<u16> = Vec::new();
    for path in paths {
        data.extend(crate::platform::wide(path.as_os_str()));
    }
    data.push(0); // terminate the list with an empty string

    let payload = COPYDATASTRUCT {
        dwData: COPYDATA_MAGIC,
        cbData: (data.len() * std::mem::size_of::<u16>()) as u32,
        lpData: data.as_ptr() as *mut std::ffi::c_void,
    };

    unsafe {
        let result = SendMessageW(
            hwnd as HWND,
            WM_COPYDATA,
            0,
            &payload as *const COPYDATASTRUCT as isize,
        );
        result == 1
    }
}

pub fn bring_to_front(hwnd: isize) {
    unsafe {
        ShowWindow(hwnd as HWND, SW_RESTORE);
        SetForegroundWindow(hwnd as HWND);
    }
}

/// Immersive dark mode for the title bar, plus matching caption/border colours
/// on Windows 11. Unknown attributes simply fail on older builds.
pub fn set_dark_mode(hwnd: isize) {
    const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1: u32 = 19;
    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_CAPTION_COLOR: u32 = 35;

    let enabled: BOOL = 1;
    unsafe {
        let hr = DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &enabled as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
        if hr < 0 {
            DwmSetWindowAttribute(
                hwnd as HWND,
                DWMWA_USE_IMMERSIVE_DARK_MODE_PRE_20H1,
                &enabled as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<BOOL>() as u32,
            );
        }

        // #1b1e24 as COLORREF (0x00BBGGRR), matching ui/appwindow.slint Theme.surface
        let caption: u32 = 0x0024_1E1B;
        DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_CAPTION_COLOR,
            &caption as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        let border: u32 = 0x0033_3346;
        DwmSetWindowAttribute(
            hwnd as HWND,
            DWMWA_BORDER_COLOR,
            &border as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

/// Native completion chime (spec §5).
pub fn beep() {
    unsafe {
        MessageBeep(MB_ICONASTERISK);
    }
}

/// Flash the taskbar button until the user looks at the window.
pub fn flash_taskbar(hwnd: isize) {
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd: hwnd as HWND,
        dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
        uCount: 3,
        dwTimeout: 0,
    };
    unsafe {
        FlashWindowEx(&info);
    }
}

// =============================================================================
//  Native dialogs
// =============================================================================

pub mod dialogs {
    use std::path::PathBuf;

    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OPENFILENAMEW, OFN_ALLOWMULTISELECT, OFN_EXPLORER, OFN_FILEMUSTEXIST,
        OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    };
    use windows_sys::Win32::UI::Shell::{
        SHBrowseForFolderW, SHGetPathFromIDListW, BROWSEINFOW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS,
    };
    use windows_sys::Win32::Foundation::HWND;

    use crate::platform::{from_wide, wide_str};

    /// `\0`-separated filter pairs, terminated by an extra `\0`.
    fn file_filter() -> Vec<u16> {
        let supported = "*.png;*.jpg;*.jpeg;*.jfif;*.webp;*.bmp;*.tif;*.tiff;*.gif;*.ico;*.svg;\
                         *.avif;*.qoi;*.mp4;*.m4v;*.mkv;*.mov;*.avi;*.webm;*.mp3;*.wav;*.flac;\
                         *.aac;*.ogg;*.m4a;*.pdf;*.docx;*.epub;*.md;*.markdown;*.txt;*.html;\
                         *.htm;*.rtf;*.csv;*.tsv;*.json;*.zip;*.7z;*.tar;*.gz;*.tgz";
        let mut filter = String::new();
        filter.push_str("All supported files");
        filter.push('\0');
        filter.push_str(supported);
        filter.push('\0');
        filter.push_str("Images");
        filter.push('\0');
        filter.push_str("*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.tif;*.tiff;*.gif;*.ico;*.svg;*.avif;*.qoi");
        filter.push('\0');
        filter.push_str("Audio & video");
        filter.push('\0');
        filter.push_str("*.mp4;*.mkv;*.mov;*.avi;*.webm;*.mp3;*.wav;*.flac;*.aac;*.ogg;*.m4a");
        filter.push('\0');
        filter.push_str("Documents");
        filter.push('\0');
        filter.push_str("*.pdf;*.docx;*.epub;*.md;*.txt;*.html;*.rtf;*.csv;*.json");
        filter.push('\0');
        filter.push_str("Archives");
        filter.push('\0');
        filter.push_str("*.zip;*.7z;*.tar;*.gz;*.tgz");
        filter.push('\0');
        filter.push_str("All files");
        filter.push('\0');
        filter.push_str("*.*");
        filter.push('\0');

        filter.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Multi-select Open dialog. Returns an empty vector when cancelled.
    pub fn pick_files(owner: isize) -> Vec<PathBuf> {
        // 64 KiB of UTF-16 covers a few thousand selected files.
        let mut buffer = vec![0u16; 32 * 1024];
        let filter = file_filter();
        let title = wide_str("Add files to convert");
        let default_ext = wide_str("");

        let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = owner as HWND;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.nFilterIndex = 1;
        ofn.lpstrFile = buffer.as_mut_ptr();
        ofn.nMaxFile = buffer.len() as u32;
        ofn.lpstrTitle = title.as_ptr();
        ofn.lpstrDefExt = default_ext.as_ptr();
        ofn.Flags = OFN_ALLOWMULTISELECT
            | OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR;

        let ok = unsafe { GetOpenFileNameW(&mut ofn) };
        if ok == 0 {
            return Vec::new();
        }

        // Single selection: one NUL-terminated path.
        // Multi selection: directory, then one file name per string, then a
        // final empty string.
        let mut parts: Vec<String> = Vec::new();
        let mut start = 0usize;
        for (i, unit) in buffer.iter().enumerate() {
            if *unit == 0 {
                if i == start {
                    break;
                }
                parts.push(from_wide(&buffer[start..=i]));
                start = i + 1;
            }
        }

        match parts.len() {
            0 => Vec::new(),
            1 => vec![PathBuf::from(&parts[0])],
            _ => {
                let dir = PathBuf::from(parts[0].clone());
                parts[1..]
                    .iter()
                    .map(|name| dir.join(name))
                    .collect::<Vec<_>>()
            }
        }
    }

    /// Browse-for-folder dialog (`SHBrowseForFolderW`).
    pub fn pick_folder(owner: isize) -> Option<PathBuf> {
        use windows_sys::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};

        unsafe {
            // S_FALSE (already initialised) is fine; S_OK is fine.
            CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED);
        }

        let title = wide_str("Choose where converted files are saved");
        let mut display = vec![0u16; 512];

        let mut bi: BROWSEINFOW = unsafe { std::mem::zeroed() };
        bi.hwndOwner = owner as HWND;
        bi.pszDisplayName = display.as_mut_ptr();
        bi.lpszTitle = title.as_ptr();
        bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;

        let pidl = unsafe { SHBrowseForFolderW(&bi) };
        if pidl.is_null() {
            return None;
        }

        let mut path = vec![0u16; 4096];
        let ok = unsafe { SHGetPathFromIDListW(pidl, path.as_mut_ptr()) };
        unsafe {
            CoTaskMemFree(pidl as *const std::ffi::c_void);
        }
        if ok == 0 {
            return None;
        }
        let text = from_wide(&path);
        if text.is_empty() {
            None
        } else {
            Some(PathBuf::from(text))
        }
    }
}
