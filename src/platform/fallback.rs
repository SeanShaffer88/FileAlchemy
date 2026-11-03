//! Non-Windows fallback.
//!
//! FileAlchemy targets Windows, but keeping a compiling fallback means the
//! engine, config and format modules can be built and unit tested on any host
//! (CI containers, code review machines). Every GUI-facing operation degrades
//! to a documented no-op.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub enum Command {
    AddPaths(Vec<PathBuf>),
    ConvertAll,
    CancelAll,
    Browse,
    OpenSettings,
    WindowReady(isize),
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

pub mod console {
    pub fn attach() -> bool {
        false
    }
    pub fn note(message: &str) {
        eprintln!("{message}");
    }
}

pub mod jobobject {
    use std::process::Child;

    pub fn assign_to_global_job(_child: &Child) {}
    pub fn terminate_all() {}
}

pub mod single_instance {
    pub struct SingleInstance {
        already_running: bool,
    }

    impl SingleInstance {
        pub fn already_running(&self) -> bool {
            self.already_running
        }
    }

    pub fn acquire() -> Option<SingleInstance> {
        Some(SingleInstance {
            already_running: false,
        })
    }
}

pub fn find_own_window(_timeout_ms: u64) -> Option<isize> {
    None
}

pub fn find_existing_window() -> Option<isize> {
    None
}

pub fn install(_queue: CommandQueue) -> Option<isize> {
    None
}

pub fn send_paths(_hwnd: isize, _paths: &[PathBuf]) -> bool {
    false
}

pub fn bring_to_front(_hwnd: isize) {}

pub fn set_dark_mode(_hwnd: isize) {}

pub fn beep() {}

pub fn flash_taskbar(_hwnd: isize) {}

pub mod dialogs {
    use std::path::PathBuf;

    pub fn pick_files(_owner: isize) -> Vec<PathBuf> {
        Vec::new()
    }

    pub fn pick_folder(_owner: isize) -> Option<PathBuf> {
        None
    }
}
