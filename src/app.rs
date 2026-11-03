//! UI controller: the only place that knows about *both* Slint and the engine.
//!
//! Threading model
//! ---------------
//! * The UI thread owns every Slint model. Nothing else may touch them.
//! * Workers only push [`EngineEvent`]s into a channel.
//! * A Slint `Timer` on the UI thread drains that channel every 80 ms, which is
//!   also when native commands (drops, hotkeys, second-instance forwards) are
//!   consumed. There is therefore no lock shared with the workers and no way for
//!   a worker to re-enter the UI.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::config::Config;
use crate::engine::formats::{
    self, CollisionPolicy, Format, OutputStrategy, Preset, Profile, Resolution,
};
use crate::engine::mime;
use crate::engine::pipeline::{Engine, EngineEvent};
use crate::engine::{human_size, TaskRequest};
use crate::paths::Paths;
use crate::platform::{self, Command, CommandQueue};
use crate::shell;
use crate::{AppWindow, Options, QueueItem};

/// How often the UI thread polls the engine and the native command queue.
const TICK: Duration = Duration::from_millis(80);
/// Safety valve for "drop a folder": nobody wants 100 000 rows by accident.
const MAX_QUEUE: usize = 5_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Face {
    Summary,
    Formats,
    Tune,
}

impl Face {
    fn id(self) -> &'static str {
        match self {
            Face::Summary => "summary",
            Face::Formats => "formats",
            Face::Tune => "tune",
        }
    }

    fn from_id(id: &str) -> Face {
        match id {
            "formats" => Face::Formats,
            "tune" => Face::Tune,
            _ => Face::Summary,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    fn id(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }
}

struct ItemState {
    id: i64,
    source: PathBuf,
    detected: mime::Detected,
    targets: &'static [Format],
    target_index: usize,
    preset: Preset,
    resolution: Resolution,
    face: Face,
    status: Status,
    progress: f32,
    indeterminate: bool,
    detail: Option<String>,
    outputs: Vec<PathBuf>,
}

impl ItemState {
    fn target(&self) -> Format {
        self.targets
            .get(self.target_index)
            .copied()
            .unwrap_or(self.targets[0])
    }
}

/// Everything the callbacks need, shared and free of `&mut self` problems.
struct Ctx {
    ui: AppWindow,
    engine: Rc<Engine>,
    paths: Rc<Paths>,
    config: Rc<RefCell<Config>>,
    items: Rc<VecModel<QueueItem>>,
    state: Rc<RefCell<Vec<ItemState>>>,
    commands: CommandQueue,
    selected: Rc<Cell<i64>>,
    next_id: Rc<Cell<i64>>,
    hwnd: Rc<Cell<isize>>,
    was_running: Rc<Cell<bool>>,
}

impl Ctx {
    // -- queue management ----------------------------------------------------

    fn enqueue(&self, inputs: Vec<PathBuf>) {
        let files = expand_paths(&inputs);
        if files.is_empty() {
            return;
        }

        let mut added = 0usize;
        {
            let mut state = self.state.borrow_mut();
            for path in files {
                if state.len() >= MAX_QUEUE {
                    break;
                }
                if state.iter().any(|item| item.source == path) {
                    continue; // already queued
                }
                // Detection failures keep the row visible with the reason
                // attached, instead of a modal dialog the user has to dismiss.
                let (detected, detection_error) = match mime::detect(&path) {
                    Ok(detected) => (detected, None),
                    Err(e) => (
                        mime::Detected {
                            kind: mime::InputKind::Txt,
                            mime: "application/octet-stream".into(),
                            confidence: mime::Confidence::Assumption,
                            note: Some(e.to_string()),
                        },
                        Some(e.to_string()),
                    ),
                };
                let targets = detected.targets();
                let default_target = detected.default_target();
                let target_index = targets
                    .iter()
                    .position(|f| *f == default_target)
                    .unwrap_or(0);
                let id = self.next_id.get();
                self.next_id.set(id + 1);
                state.push(ItemState {
                    id,
                    source: path,
                    detected,
                    targets,
                    target_index,
                    preset: Preset::Balanced,
                    resolution: Resolution::Original,
                    face: Face::Summary,
                    status: if detection_error.is_some() {
                        Status::Failed
                    } else {
                        Status::Queued
                    },
                    progress: 0.0,
                    indeterminate: false,
                    detail: detection_error,
                    outputs: Vec::new(),
                });
                added += 1;
            }
        }
        if added > 0 {
            self.sync_model();
            self.update_summary();
        }
    }

    fn remove(&self, id: i64) {
        if self.is_running(id) {
            self.engine.cancel(id as u64);
        }
        let mut state = self.state.borrow_mut();
        if let Some(index) = state.iter().position(|item| item.id == id) {
            state.remove(index);
        }
        drop(state);
        self.sync_model();
        self.update_summary();
    }

    fn clear(&self) {
        self.engine.cancel_all();
        let ids: Vec<i64> = self.state.borrow().iter().map(|i| i.id).collect();
        for id in ids {
            self.engine.cancel(id as u64);
        }
        self.state.borrow_mut().clear();
        self.ui.set_running(false);
        self.was_running.set(false);
        self.sync_model();
        self.update_summary();
    }

    fn is_running(&self, id: i64) -> bool {
        self.state
            .borrow()
            .iter()
            .any(|item| item.id == id && item.status == Status::Running)
    }

    fn select(&self, id: i64) {
        self.selected.set(id);
        self.sync_model();
    }

    fn set_face(&self, id: i64, face: &str) {
        let mut state = self.state.borrow_mut();
        if let Some(item) = state.iter_mut().find(|i| i.id == id) {
            item.face = Face::from_id(face);
        }
    }

    fn set_target(&self, id: i64, index: usize) {
        let mut state = self.state.borrow_mut();
        if let Some(item) = state.iter_mut().find(|i| i.id == id) {
            if index < item.targets.len() {
                item.target_index = index;
                item.face = Face::Summary;
            }
        }
        drop(state);
        self.sync_model();
        self.update_summary();
    }

    fn set_preset(&self, id: i64, index: usize) {
        let mut state = self.state.borrow_mut();
        if let Some(item) = state.iter_mut().find(|i| i.id == id) {
            item.preset = Preset::from_index(index);
        }
        drop(state);
        self.sync_model();
    }

    fn set_resolution(&self, id: i64, index: usize) {
        let mut state = self.state.borrow_mut();
        if let Some(item) = state.iter_mut().find(|i| i.id == id) {
            item.resolution = Resolution::from_index(index);
        }
        drop(state);
        self.sync_model();
    }

    // -- conversion ----------------------------------------------------------

    fn convert_all(&self) {
        if self.ui.get_running() {
            return;
        }
        let collision = self.config.borrow().collision_policy;
        let custom_dir = self.custom_output_dir();
        let purge = self.config.borrow().cleanup.purge_temp_on_cancel;

        let mut requests = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            for item in state.iter_mut() {
                if item.status == Status::Running {
                    continue;
                }
                let output_dir = match self.config.borrow().output_strategy {
                    OutputStrategy::SameAsSource => item
                        .source
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| self.paths.data_dir.clone()),
                    OutputStrategy::CustomDir => custom_dir.clone(),
                };
                item.status = Status::Queued;
                item.progress = 0.0;
                item.indeterminate = false;
                item.detail = None;
                item.outputs.clear();

                requests.push(TaskRequest {
                    id: item.id as u64,
                    source: item.source.clone(),
                    detected: item.detected.clone(),
                    target: item.target(),
                    preset: item.preset,
                    resolution: item.resolution,
                    output_dir,
                    collision,
                    purge_on_cancel: purge,
                });
            }
        }

        if requests.is_empty() {
            return;
        }

        self.ui.set_running(true);
        self.was_running.set(true);
        for request in requests {
            let id = request.id;
            if let Err(e) = self.engine.submit(request) {
                self.mark_failed(id, e.to_string());
            }
        }
        self.sync_model();
        self.update_summary();
    }

    fn cancel_all(&self) {
        self.engine.cancel_all();
        let mut state = self.state.borrow_mut();
        for item in state.iter_mut() {
            if item.status == Status::Queued || item.status == Status::Running {
                item.status = Status::Cancelled;
                item.detail = Some("Cancelled - partial files were removed".into());
                item.indeterminate = false;
            }
        }
        drop(state);
        self.ui.set_running(false);
        self.was_running.set(false);
        self.sync_model();
        self.update_summary();
    }

    fn custom_output_dir(&self) -> PathBuf {
        let config = self.config.borrow();
        if config.custom_output_dir.trim().is_empty() {
            self.paths.data_dir.clone()
        } else {
            PathBuf::from(config.custom_output_dir.trim())
        }
    }

    fn browse(&self) {
        let hwnd = self.hwnd.get();
        let files = platform::dialogs::pick_files(hwnd);
        if !files.is_empty() {
            self.enqueue(files);
        }
    }

    fn choose_output_dir(&self) {
        let hwnd = self.hwnd.get();
        let Some(folder) = platform::dialogs::pick_folder(hwnd) else {
            return;
        };
        {
            let mut config = self.config.borrow_mut();
            config.custom_output_dir = folder.display().to_string();
            config.output_strategy = OutputStrategy::CustomDir;
        }
        self.ui
            .set_custom_output_path(SharedString::from(folder.display().to_string()));
        self.ui.set_custom_output(true);
        self.persist_config();
    }

    fn set_custom_output(&self, custom: bool) {
        if custom && self.ui.get_custom_output_path().is_empty() {
            self.choose_output_dir();
            return;
        }
        self.ui.set_custom_output(custom);
        {
            let mut config = self.config.borrow_mut();
            config.output_strategy = if custom {
                OutputStrategy::CustomDir
            } else {
                OutputStrategy::SameAsSource
            };
        }
        self.persist_config();
    }

    fn set_collision(&self, index: usize) {
        let policy = if index == 1 {
            CollisionPolicy::Overwrite
        } else {
            CollisionPolicy::AutoRename
        };
        self.config.borrow_mut().collision_policy = policy;
        self.ui.set_collision_index(index as i32);
        self.persist_config();
    }

    // -- settings ------------------------------------------------------------

    fn save_settings(&self) {
        let ui = &self.ui;
        {
            let mut config = self.config.borrow_mut();
            config.max_concurrent_tasks =
                Config::concurrency_from_index(ui.get_concurrency_index() as usize);
            config.notifications.play_sound_on_complete = ui.get_play_sound();
            config.notifications.flash_taskbar_on_complete = ui.get_flash_taskbar();
            config.cleanup.purge_temp_on_cancel = ui.get_purge_temp();
            let logical = ui.window().size().to_logical(ui.window().scale_factor());
            config.window.width = logical.width.round() as i32;
            config.window.height = logical.height.round() as i32;
        }

        // The Explorer menu is an OS-level side effect, so it is applied
        // immediately rather than only on the next start.
        let wanted = ui.get_shell_menu();
        let installed = shell::is_installed();
        if wanted != installed {
            let exe = self.paths.exe_path();
            match shell::set_enabled(wanted, &exe) {
                shell::ShellMenuChange::Installed => {
                    self.config.borrow_mut().shell_integration.enabled = true;
                }
                shell::ShellMenuChange::Removed => {
                    self.config.borrow_mut().shell_integration.enabled = false;
                }
                shell::ShellMenuChange::Failed(message) => {
                    platform::console::note(&format!("Explorer menu: {message}"));
                    // Reflect reality back into the toggle.
                    ui.set_shell_menu(installed);
                    self.config.borrow_mut().shell_integration.enabled = installed;
                }
            }
        }

        self.persist_config();
    }

    fn persist_config(&self) {
        let config = self.config.borrow().clone();
        if let Err(e) = config.save(&self.paths.config_file) {
            platform::console::note(&format!("Could not save config.json: {e}"));
        }
    }

    // -- event pump ----------------------------------------------------------

    fn tick(&self) {
        // 1. native commands (drops, hotkeys, second instance)
        loop {
            let command = {
                let mut queue = match self.commands.lock() {
                    Ok(queue) => queue,
                    Err(_) => break,
                };
                queue.pop_front()
            };
            match command {
                Some(command) => self.handle_command(command),
                None => break,
            }
        }

        // 2. engine events
        let mut finished_any = false;
        while let Some(event) = self.engine.try_recv() {
            match event {
                EngineEvent::Started { id } => {
                    self.with_item(id, |item| {
                        item.status = Status::Running;
                        item.progress = 0.01;
                        item.indeterminate = false;
                        item.detail = None;
                    });
                }
                EngineEvent::Progress { id, fraction } => {
                    self.with_item(id, |item| match fraction {
                        Some(value) => {
                            item.indeterminate = false;
                            item.progress = value.clamp(0.0, 1.0);
                        }
                        None => item.indeterminate = true,
                    });
                }
                EngineEvent::Done { id, outputs } => {
                    finished_any = true;
                    self.with_item(id, |item| {
                        item.status = Status::Done;
                        item.progress = 1.0;
                        item.indeterminate = false;
                        item.outputs = outputs;
                        item.detail = None;
                    });
                }
                EngineEvent::Failed { id, message } => {
                    finished_any = true;
                    self.mark_failed(id, message);
                }
                EngineEvent::Cancelled { id } => {
                    finished_any = true;
                    self.with_item(id, |item| {
                        item.status = Status::Cancelled;
                        item.indeterminate = false;
                        item.detail = Some("Cancelled - partial files were removed".into());
                    });
                }
            }
        }

        if finished_any {
            self.sync_model();
            self.update_summary();
        }

        // 3. completion notifications (once per queue)
        if self.was_running.get() && !self.any_active() {
            self.was_running.set(false);
            self.ui.set_running(false);
            self.update_summary();
            self.notify_completion();
        }
    }

    fn handle_command(&self, command: Command) {
        match command {
            Command::AddPaths(paths) => self.enqueue(paths),
            Command::ConvertAll => {
                if !self.ui.get_settings_open() {
                    self.convert_all();
                }
            }
            Command::CancelAll => {
                if self.ui.get_settings_open() {
                    self.ui.set_settings_open(false);
                } else if self.ui.get_running() {
                    self.cancel_all();
                }
            }
            Command::Browse => self.browse(),
            Command::OpenSettings => self.ui.set_settings_open(!self.ui.get_settings_open()),
            Command::WindowReady(hwnd) => {
                self.hwnd.set(hwnd);
            }
            Command::BringToFront => {
                self.ui.window().request_redraw();
            }
        }
    }

    fn any_active(&self) -> bool {
        self.state
            .borrow()
            .iter()
            .any(|item| matches!(item.status, Status::Running | Status::Queued))
    }

    fn notify_completion(&self) {
        let config = self.config.borrow();
        if config.notifications.play_sound_on_complete {
            platform::beep();
        }
        if config.notifications.flash_taskbar_on_complete {
            if let Some(hwnd) = self.hwnd.get().checked_abs() {
                if hwnd != 0 {
                    platform::flash_taskbar(hwnd);
                }
            }
        }
    }

    fn mark_failed(&self, id: u64, message: String) {
        self.with_item(id, |item| {
            item.status = Status::Failed;
            item.indeterminate = false;
            item.detail = Some(message);
        });
    }

    fn with_item(&self, id: u64, mutate: impl FnOnce(&mut ItemState)) {
        let mut state = self.state.borrow_mut();
        if let Some(item) = state.iter_mut().find(|item| item.id as u64 == id) {
            mutate(item);
        }
    }

    // -- model sync ----------------------------------------------------------

    fn sync_model(&self) {
        let selected = self.selected.get();
        let state = self.state.borrow();
        let items = &self.items;

        while items.row_count() > state.len() {
            let last = items.row_count() - 1;
            items.remove(last);
        }
        for (index, item) in state.iter().enumerate() {
            let row = self.build_row(item, selected);
            if index < items.row_count() {
                items.set_row_data(index, row);
            } else {
                items.push(row);
            }
        }
    }

    fn build_row(&self, item: &ItemState, selected_id: i64) -> QueueItem {
        let size = crate::engine::file_size(&item.source);
        let detail = match item.status {
            Status::Failed => item
                .detail
                .clone()
                .unwrap_or_else(|| "Conversion failed".into()),
            Status::Cancelled => "Cancelled - partial files were removed".into(),
            Status::Done => describe_outputs(&item.outputs),
            _ => format!(
                "{} · {} · {}",
                human_size(size),
                item.detected.display_type(),
                item.detected.confidence.description()
            ),
        };

        let status_label = match item.status {
            Status::Queued => "Ready".to_string(),
            Status::Running => format!("{}%", (item.progress * 100.0).round() as i32),
            Status::Done => "Done".to_string(),
            Status::Failed => "Failed".to_string(),
            Status::Cancelled => "Cancelled".to_string(),
        };

        QueueItem {
            id: item.id as i32,
            name: SharedString::from(
                item.source
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            detail: SharedString::from(detail),
            mime: SharedString::from(item.detected.mime.clone()),
            category: SharedString::from(item.detected.category().id()),
            category_label: SharedString::from(item.detected.category().label()),
            format_profile: SharedString::from(item.detected.profile().id()),
            target: SharedString::from(item.target().label()),
            target_index: item.target_index as i32,
            preset_index: item.preset.index() as i32,
            resolution_index: item.resolution.index() as i32,
            face: SharedString::from(item.face.id()),
            progress: item.progress,
            indeterminate: item.indeterminate,
            status: SharedString::from(item.status.id()),
            status_label: SharedString::from(status_label),
            selected: item.id == selected_id,
        }
    }

    fn update_summary(&self) {
        let state = self.state.borrow();
        if state.is_empty() {
            self.ui.set_summary(SharedString::from("No files queued"));
            return;
        }
        let done = state.iter().filter(|i| i.status == Status::Done).count();
        let failed = state.iter().filter(|i| i.status == Status::Failed).count();
        let cancelled = state
            .iter()
            .filter(|i| i.status == Status::Cancelled)
            .count();
        let pending = state.len() - done - failed - cancelled;

        let mut parts = vec![format!(
            "{} file{}",
            state.len(),
            if state.len() == 1 { "" } else { "s" }
        )];
        if pending > 0 {
            parts.push(format!("{pending} waiting"));
        }
        if done > 0 {
            parts.push(format!("{done} converted"));
        }
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        if cancelled > 0 {
            parts.push(format!("{cancelled} cancelled"));
        }
        self.ui.set_summary(SharedString::from(parts.join(" · ")));
    }
}

fn describe_outputs(outputs: &[PathBuf]) -> String {
    match outputs.len() {
        0 => "Finished".to_string(),
        1 => format!("Saved · {}", outputs[0].display()),
        n => format!(
            "Saved {n} files · {} … {}",
            outputs
                .first()
                .map(|p| p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned())
                .unwrap_or_default(),
            outputs
                .last()
                .map(|p| p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned())
                .unwrap_or_default()
        ),
    }
}

/// Expand folders into the files inside them (depth-limited, size-limited).
fn expand_paths(inputs: &[PathBuf]) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 12;

    fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > MAX_DEPTH || out.len() >= MAX_QUEUE {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if out.len() >= MAX_QUEUE {
                return;
            }
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => walk(&path, depth + 1, out),
                Ok(kind) if kind.is_file() => out.push(path),
                _ => {}
            }
        }
    }

    let mut out = Vec::new();
    for input in inputs {
        if out.len() >= MAX_QUEUE {
            break;
        }
        if input.is_dir() {
            walk(input, 0, &mut out);
        } else if input.is_file() {
            out.push(input.clone());
        }
    }
    out
}

/// Owns the window, the timer and the shared context.
pub struct App {
    ctx: Rc<Ctx>,
    _timer: Timer,
}

impl App {
    pub fn new(
        paths: Paths,
        config: Config,
        engine: Engine,
        commands: CommandQueue,
    ) -> Result<App, slint::PlatformError> {
        let ui = AppWindow::new()?;

        // Remembered geometry. `config` is in logical pixels, the window API
        // wants physical ones, so the current scale factor is applied.
        let scale = ui.window().scale_factor();
        let width = (config.window.width.max(720) as f32 * scale).round() as u32;
        let height = (config.window.height.max(480) as f32 * scale).round() as u32;
        ui.window()
            .set_size(slint::PhysicalSize::new(width, height));

        let model: Rc<VecModel<QueueItem>> = Rc::new(VecModel::from(Vec::<QueueItem>::new()));
        ui.set_items(ModelRc::from(model.clone()));

        fill_options(&ui);

        let shell_installed = shell::is_installed();
        ui.set_collision_index(match config.collision_policy {
            CollisionPolicy::Overwrite => 1,
            CollisionPolicy::AutoRename => 0,
        });
        ui.set_concurrency_index(config.concurrency_index() as i32);
        ui.set_shell_menu(shell_installed || config.shell_integration.enabled);
        ui.set_play_sound(config.notifications.play_sound_on_complete);
        ui.set_flash_taskbar(config.notifications.flash_taskbar_on_complete);
        ui.set_purge_temp(config.cleanup.purge_temp_on_cancel);
        ui.set_custom_output(config.output_strategy == OutputStrategy::CustomDir);
        ui.set_custom_output_path(SharedString::from(config.custom_output_dir.clone()));

        let ctx = Rc::new(Ctx {
            ui,
            engine: Rc::new(engine),
            paths: Rc::new(paths),
            config: Rc::new(RefCell::new(config)),
            items: model,
            state: Rc::new(RefCell::new(Vec::new())),
            commands,
            selected: Rc::new(Cell::new(-1)),
            next_id: Rc::new(Cell::new(1)),
            hwnd: Rc::new(Cell::new(0)),
            was_running: Rc::new(Cell::new(false)),
        });

        wire_callbacks(&ctx);

        let timer = Timer::default();
        let tick_ctx = Rc::downgrade(&ctx);
        timer.start(TimerMode::Repeated, TICK, move || {
            if let Some(ctx) = tick_ctx.upgrade() {
                ctx.tick();
            }
        });

        Ok(App { ctx, _timer: timer })
    }

    pub fn ui(&self) -> &AppWindow {
        &self.ctx.ui
    }

    /// Queue files that arrived on the command line.
    pub fn enqueue(&self, paths: Vec<PathBuf>) {
        self.ctx.enqueue(paths);
    }
}

fn fill_options(ui: &AppWindow) {
    fn labels(profile: Profile) -> ModelRc<SharedString> {
        let values: Vec<SharedString> = profile
            .targets()
            .iter()
            .map(|f| SharedString::from(f.label()))
            .collect();
        ModelRc::from(Rc::new(VecModel::from(values)))
    }

    fn helps(profile: Profile) -> ModelRc<SharedString> {
        let values: Vec<SharedString> = profile
            .targets()
            .iter()
            .map(|f| SharedString::from(f.help()))
            .collect();
        ModelRc::from(Rc::new(VecModel::from(values)))
    }

    fn strings(values: impl Iterator<Item = SharedString>) -> ModelRc<SharedString> {
        ModelRc::from(Rc::new(VecModel::from(values.collect::<Vec<_>>())))
    }

    let options = ui.global::<Options>();
    options.set_image_formats(labels(Profile::Image));
    options.set_video_formats(labels(Profile::Video));
    options.set_audio_formats(labels(Profile::Audio));
    options.set_text_formats(labels(Profile::Text));
    options.set_pdf_formats(labels(Profile::Pdf));
    options.set_archive_formats(labels(Profile::Archive));

    // The explanations are the UI's only documentation of what each format
    // is, so they are generated from the same table that drives the labels.
    options.set_image_format_helps(helps(Profile::Image));
    options.set_video_format_helps(helps(Profile::Video));
    options.set_audio_format_helps(helps(Profile::Audio));
    options.set_text_format_helps(helps(Profile::Text));
    options.set_pdf_format_helps(helps(Profile::Pdf));
    options.set_archive_format_helps(helps(Profile::Archive));

    options.set_presets(strings(
        formats::PRESETS
            .iter()
            .map(|p| SharedString::from(p.label())),
    ));
    options.set_preset_helps(strings(
        formats::PRESETS
            .iter()
            .map(|p| SharedString::from(p.help())),
    ));
    options.set_resolutions(strings(
        formats::RESOLUTIONS
            .iter()
            .map(|r| SharedString::from(r.label())),
    ));
    options.set_resolution_helps(strings(
        formats::RESOLUTIONS
            .iter()
            .map(|r| SharedString::from(r.help())),
    ));
}

fn wire_callbacks(ctx: &Rc<Ctx>) {
    let ui = &ctx.ui;

    {
        let ctx = Rc::downgrade(ctx);
        ui.on_browse_files(move || {
            if let Some(ctx) = ctx.upgrade() {
                ctx.browse();
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_choose_output_dir(move || {
            if let Some(ctx) = ctx.upgrade() {
                ctx.choose_output_dir();
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_convert_all(move || {
            if let Some(ctx) = ctx.upgrade() {
                ctx.convert_all();
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_cancel_all(move || {
            if let Some(ctx) = ctx.upgrade() {
                ctx.cancel_all();
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_clear_queue(move || {
            if let Some(ctx) = ctx.upgrade() {
                ctx.clear();
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_remove_item(move |id| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.remove(id as i64);
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_select_item(move |id| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.select(id as i64);
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_set_face(move |id, face| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.set_face(id as i64, face.as_str());
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_set_target(move |id, index| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.set_target(id as i64, index as usize);
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_set_preset(move |id, index| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.set_preset(id as i64, index as usize);
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_set_resolution(move |id, index| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.set_resolution(id as i64, index as usize);
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_set_collision(move |index| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.set_collision(index as usize);
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_set_custom_output(move |custom| {
            if let Some(ctx) = ctx.upgrade() {
                ctx.set_custom_output(custom);
            }
        });
    }
    {
        let ctx = Rc::downgrade(ctx);
        ui.on_save_settings(move || {
            if let Some(ctx) = ctx.upgrade() {
                ctx.save_settings();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_round_trip() {
        for face in [Face::Summary, Face::Formats, Face::Tune] {
            assert_eq!(Face::from_id(face.id()), face);
        }
        assert_eq!(Face::from_id("nonsense"), Face::Summary);
    }

    #[test]
    fn expand_paths_walks_folders() {
        let root = std::env::temp_dir().join(format!("filealchemy-expand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("a.png"), b"x").unwrap();
        std::fs::write(root.join("nested/b.txt"), b"x").unwrap();

        let files = expand_paths(&[root.clone()]);
        assert_eq!(files.len(), 2, "{files:?}");

        // Single files pass through untouched.
        let single = expand_paths(&[root.join("a.png")]);
        assert_eq!(single.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn describe_outputs_mentions_the_file() {
        let one = describe_outputs(&[PathBuf::from(r"C:\out\a.png")]);
        assert!(one.contains("a.png"), "{one}");
        let many = describe_outputs(&[
            PathBuf::from("a.png"),
            PathBuf::from("b.png"),
            PathBuf::from("c.png"),
        ]);
        assert!(many.contains("3 files"), "{many}");
    }
}
