//! FileAlchemy — offline, portable universal file converter.
//!
//! Startup order (each step is deliberate):
//!
//!  1. attach to the parent console *if any* (so `--install-shell-menu` from a
//!     terminal is not silent, while launching from Explorer stays windowless);
//!  2. resolve portable vs installed data folders;
//!  3. handle `--help`, `--version`, `--install-shell-menu`,
//!     `--uninstall-shell-menu` and exit — these never create a window;
//!  4. take the single-instance mutex; if another instance owns it, forward the
//!     file arguments over `WM_COPYDATA` and exit;
//!  5. purge the scratch folder (safe: we are the only instance);
//!  6. start the software-rendered Slint UI and the conversion worker pool;
//!  7. install the WndProc hooks (drops / hotkeys / dark mode) once the HWND
//!     exists, on a helper thread so the window paints immediately.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod engine;
mod paths;
mod platform;
mod shell;

slint::include_modules!();

use std::path::PathBuf;
use std::process::ExitCode;

use slint::ComponentHandle;

use app::App;
use config::Config;
use engine::pipeline::Engine;

const USAGE: &str = "\
FileAlchemy - offline universal file converter

USAGE:
    FileAlchemy.exe [FILES...]

OPTIONS:
    --install-shell-menu     Add \"Convert with FileAlchemy\" to the Explorer
                             right-click menu (current user, no admin rights)
    --uninstall-shell-menu   Remove that menu entry again
    -h, --help               Show this help
    -V, --version            Show the version

FILES may be files or folders; folders are scanned and their supported files
are added to the queue. Everything is converted on this PC: FileAlchemy makes
no network requests and has no telemetry.";

fn main() -> ExitCode {
    // Borrow the parent console when launched from a terminal. From Explorer
    // there is no parent console and this simply fails, which is fine.
    platform::console::attach();

    match run() {
        Ok(code) => code,
        Err(message) => {
            platform::console::note(&format!("FileAlchemy: {message}"));
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let paths = paths::resolve();

    // Portable builds work no matter what the "current directory" is when the
    // shell starts us, but relative CLI arguments should still resolve next to
    // the executable.
    if paths.portable {
        let _ = std::env::set_current_dir(&paths.exe_dir);
    }
    paths
        .ensure()
        .map_err(|e| format!("cannot create \"{}\": {e}", paths.data_dir.display()))?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut install_menu = false;
    let mut uninstall_menu = false;

    for arg in &args {
        match arg.as_str() {
            "--install-shell-menu" => install_menu = true,
            "--uninstall-shell-menu" => uninstall_menu = true,
            "-h" | "--help" | "/?" => {
                platform::console::note(USAGE);
                return Ok(ExitCode::SUCCESS);
            }
            "-V" | "--version" => {
                platform::console::note(&format!("FileAlchemy {}", env!("CARGO_PKG_VERSION")));
                return Ok(ExitCode::SUCCESS);
            }
            other if other.starts_with('-') => {
                platform::console::note(&format!("Unknown option: {other}\n\n{USAGE}"));
                return Ok(ExitCode::from(2));
            }
            other => files.push(PathBuf::from(other)),
        }
    }

    if install_menu || uninstall_menu {
        return Ok(handle_shell_menu(&paths, install_menu));
    }

    // ---- single instance ---------------------------------------------------
    let instance = platform::single_instance::acquire();
    if let Some(instance) = instance.as_ref() {
        if instance.already_running() {
            if let Some(hwnd) = platform::find_existing_window() {
                let forwarded = platform::send_paths(hwnd, &files);
                platform::bring_to_front(hwnd);
                if !forwarded {
                    platform::console::note(
                        "FileAlchemy is already running but did not accept the files.",
                    );
                }
            }
            return Ok(ExitCode::SUCCESS);
        }
    }

    // ---- configuration -----------------------------------------------------
    let (config, warning) = Config::load_or_default(&paths.config_file);
    if let Some(warning) = warning {
        platform::console::note(&warning);
    }
    let _ = config.save(&paths.config_file);

    // ---- engine ------------------------------------------------------------
    // `Scratch::open` purges leftovers from a previous crash. That is safe
    // precisely because the single-instance mutex is already held.
    let engine_ctx = engine::pipeline::build_context(&paths.temp_dir, &paths.exe_dir)
        .map_err(|e| format!("cannot prepare the temp folder: {e}"))?;
    let engine = Engine::new(engine_ctx, config.max_concurrent_tasks);
    let worker_count = engine.threads();

    // ---- UI ----------------------------------------------------------------
    // Software rendering: no GPU, no OpenGL, no Direct3D driver to blame. This
    // is the one backend that behaves identically on a 2010 laptop, inside a
    // Remote Desktop session and on a VM without 3D acceleration.
    std::env::set_var("SLINT_BACKEND", "winit-software");

    let commands = platform::new_command_queue();
    let app = App::new(paths.clone(), config, engine, commands.clone())
        .map_err(|e| format!("cannot create the window: {e}"))?;

    app.ui()
        .show()
        .map_err(|e| format!("cannot show the window: {e}"))?;

    if !files.is_empty() {
        app.enqueue(files);
    }

    // ---- native hooks ------------------------------------------------------
    // Runs off-thread: the HWND appears asynchronously once winit has created
    // it, and we must not block the first paint while polling for it.
    {
        let commands = commands.clone();
        std::thread::Builder::new()
            .name("filealchemy-hooks".into())
            .spawn(move || match platform::install(commands.clone()) {
                Some(hwnd) => platform::push(&commands, platform::Command::WindowReady(hwnd)),
                None => platform::console::note(
                    "Could not attach the native window hooks; drag & drop and global \
                     hotkeys are unavailable. The UI still works.",
                ),
            })
            .map_err(|e| format!("cannot start the window hook thread: {e}"))?;
    }

    if let Some(missing) = missing_tools(&paths) {
        platform::console::note(&missing);
    }
    platform::console::note(&format!(
        "FileAlchemy {} ready · {} worker{} · data folder: {}",
        env!("CARGO_PKG_VERSION"),
        worker_count,
        if worker_count == 1 { "" } else { "s" },
        paths.data_dir.display()
    ));

    slint::run_event_loop().map_err(|e| format!("the window loop stopped: {e}"))?;

    // Dropping the app joins the workers (cancelling anything still running)
    // and the scratch directory is purged on the way out.
    drop(app);
    platform::jobobject::terminate_all();
    Ok(ExitCode::SUCCESS)
}

fn handle_shell_menu(paths: &paths::Paths, install: bool) -> ExitCode {
    let exe = std::env::current_exe().unwrap_or_else(|_| paths.exe_path());
    let outcome = shell::set_enabled(install, &exe);

    let (config, _) = Config::load_or_default(&paths.config_file);
    let mut config = config;
    match &outcome {
        shell::ShellMenuChange::Installed => {
            config.shell_integration.enabled = true;
            platform::console::note(
                "Added \"Convert with FileAlchemy\" to the Explorer context menu.",
            );
        }
        shell::ShellMenuChange::Removed => {
            config.shell_integration.enabled = false;
            platform::console::note("Removed the Explorer context menu entry.");
        }
        shell::ShellMenuChange::Failed(message) => {
            platform::console::note(&format!("Explorer menu: {message}"));
            return ExitCode::from(1);
        }
    }
    if let Err(e) = config.save(&paths.config_file) {
        platform::console::note(&format!("Could not save config.json: {e}"));
    }
    ExitCode::SUCCESS
}

fn missing_tools(paths: &paths::Paths) -> Option<String> {
    let tools = engine::external::ToolPaths::resolve(&paths.exe_dir);
    let missing = tools.missing();
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "Some bundled engines were not found in \"{}\": {}. \
             Conversions that need them will report a clear error instead of failing silently.",
            tools.bin_dir.display(),
            missing.join(", ")
        ))
    }
}
