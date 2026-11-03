//! Worker pool, task registry and cancellation.
//!
//! The UI thread never blocks on a conversion. It calls [`Engine::submit`] and
//! then drains [`EngineEvent`]s on a timer, which keeps the window responsive
//! while four ffmpeg jobs are chewing through a video queue.
//!
//! Cancellation is cooperative *and* preemptive where it matters:
//! * queued jobs are skipped when their token is already set,
//! * running in-process work checks the token at every stage boundary,
//! * running child processes are killed within 60 ms by [`super::external`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};

use crate::engine::formats::Format;
use crate::engine::scratch::{self, Scratch};
use crate::engine::{
    new_cancel_token, CancelToken, EngineContext, EngineError, TaskRequest,
};

/// Events produced by workers, consumed by the UI thread.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// The worker picked the job up (after the queue delay).
    Started { id: u64 },
    /// `fraction` is `None` while the engine cannot measure progress.
    Progress { id: u64, fraction: Option<f32> },
    Done { id: u64, outputs: Vec<PathBuf> },
    Failed { id: u64, message: String },
    Cancelled { id: u64 },
}

struct Job {
    req: TaskRequest,
    token: CancelToken,
}

/// The conversion engine. Cheap to own; all heavy state lives in the workers.
pub struct Engine {
    ctx: Arc<EngineContext>,
    tx: Sender<EngineEvent>,
    rx: Receiver<EngineEvent>,
    job_tx: Option<Sender<Job>>,
    tokens: Arc<Mutex<HashMap<u64, CancelToken>>>,
    workers: Vec<JoinHandle<()>>,
    threads: usize,
}

impl Engine {
    pub fn new(ctx: Arc<EngineContext>, threads: usize) -> Engine {
        let threads = threads.clamp(1, 16);
        let (tx, rx) = bounded(4096);
        let (job_tx, job_rx) = unbounded::<Job>();
        let tokens: Arc<Mutex<HashMap<u64, CancelToken>>> = Arc::new(Mutex::new(HashMap::new()));

        let mut workers = Vec::with_capacity(threads);
        for index in 0..threads {
            let job_rx = job_rx.clone();
            let tx = tx.clone();
            let ctx = Arc::clone(&ctx);
            let tokens = Arc::clone(&tokens);
            let handle = std::thread::Builder::new()
                .name(format!("filealchemy-worker-{index}"))
                .spawn(move || worker_loop(job_rx, tx, ctx, tokens))
                .expect("failed to spawn conversion worker");
            workers.push(handle);
        }
        drop(job_rx);

        Engine {
            ctx,
            tx,
            rx,
            job_tx: Some(job_tx),
            tokens,
            workers,
            threads,
        }
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Queue a conversion. The request carries its own output directory, so
    /// the worker needs no access to settings.
    pub fn submit(&self, req: TaskRequest) -> Result<(), EngineError> {
        let token = new_cancel_token();
        self.tokens.lock().unwrap().insert(req.id, Arc::clone(&token));
        let job = Job { req, token };
        self.job_tx
            .as_ref()
            .ok_or_else(|| EngineError::Internal("engine is shutting down".into()))?
            .send(job)
            .map_err(|_| EngineError::Internal("conversion queue is closed".into()))
    }

    /// Request cancellation of one job (running or still queued).
    pub fn cancel(&self, id: u64) {
        if let Some(token) = self.tokens.lock().unwrap().get(&id) {
            token.store(true, Ordering::SeqCst);
        }
    }

    /// Request cancellation of everything (Esc / Cancel All).
    pub fn cancel_all(&self) {
        for token in self.tokens.lock().unwrap().values() {
            token.store(true, Ordering::SeqCst);
        }
    }

    /// Number of jobs currently registered (running or queued).
    #[allow(dead_code)]
    pub fn active_tasks(&self) -> usize {
        self.tokens.lock().unwrap().len()
    }

    /// Non-blocking event drain, called from the UI timer.
    pub fn try_recv(&self) -> Option<EngineEvent> {
        self.rx.try_recv().ok()
    }

    /// Blocking receive with a timeout; used by the tests and by tooling.
    #[allow(dead_code)]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<EngineEvent> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Stop accepting work, cancel everything, and wait for workers to unwind.
    pub fn shutdown(&mut self) {
        self.cancel_all();
        self.job_tx.take();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        // Anything left in the scratch area belongs to a cancelled task.
        let _ = self.ctx.scratch.purge_all();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if !self.workers.is_empty() {
            self.shutdown();
        }
    }
}

fn worker_loop(
    job_rx: Receiver<Job>,
    tx: Sender<EngineEvent>,
    ctx: Arc<EngineContext>,
    tokens: Arc<Mutex<HashMap<u64, CancelToken>>>,
) {
    while let Ok(job) = job_rx.recv() {
        let id = job.req.id;

        // Skipped before it starts: the user removed it while it was queued.
        if job.token.load(Ordering::SeqCst) {
            tokens.lock().unwrap().remove(&id);
            let _ = tx.send(EngineEvent::Cancelled { id });
            ctx.scratch.release_task(id);
            continue;
        }

        let _ = tx.send(EngineEvent::Started { id });

        let mut last_emit: Option<Instant> = None;
        let mut progress = |fraction: Option<f32>| {
            let now = Instant::now();
            let due = match last_emit {
                None => true,
                Some(t) => now.duration_since(t) >= Duration::from_millis(80),
            };
            if due {
                last_emit = Some(now);
                let _ = tx.send(EngineEvent::Progress { id, fraction });
            }
        };

        let result = crate::engine::convert(&job.req, &ctx, &job.token, &mut progress);

        match result {
            Ok(_staged) if job.token.load(Ordering::SeqCst) => {
                // Cancelled between the final check and placement.
                let _ = tx.send(EngineEvent::Cancelled { id });
            }
            Ok(temps) => match place_outputs(&job.req, &temps) {
                Ok(outputs) => {
                    let _ = tx.send(EngineEvent::Done { id, outputs });
                }
                Err(e) => {
                    let _ = tx.send(EngineEvent::Failed {
                        id,
                        message: e.to_string(),
                    });
                }
            },
            Err(EngineError::Cancelled) => {
                let _ = tx.send(EngineEvent::Cancelled { id });
            }
            Err(e) => {
                let _ = tx.send(EngineEvent::Failed {
                    id,
                    message: e.to_string(),
                });
            }
        }

        // Always scrub: success, failure or cancellation.
        ctx.scratch.release_task(id);
        tokens.lock().unwrap().remove(&id);
    }
}

/// Move every staged artefact into the destination folder.
fn place_outputs(req: &TaskRequest, temps: &[PathBuf]) -> Result<Vec<PathBuf>, EngineError> {
    if req.target == Format::ExtractDir {
        let staging = temps.first().ok_or_else(|| {
            EngineError::Internal("archive produced no extraction directory".into())
        })?;
        let placed = scratch::place_dir(staging, &req.output_dir, &req.stem(), req.collision)?;
        return Ok(vec![placed]);
    }

    let ext = req.target.extension();
    let mut outputs = Vec::with_capacity(temps.len());
    for (index, temp) in temps.iter().enumerate() {
        let stem = page_stem(&req.stem(), index);
        outputs.push(scratch::place_file(
            temp,
            &req.output_dir,
            &stem,
            ext,
            req.collision,
        )?);
    }
    Ok(outputs)
}

/// `report` for the first result, `report_p2`, `report_p3`, ... for the rest
/// (multi-page PDF rasterisation).
fn page_stem(stem: &str, index: usize) -> String {
    if index == 0 {
        stem.to_string()
    } else {
        format!("{stem}_p{}", index + 1)
    }
}

/// Convenience wrapper used by the UI and by tests.
pub fn build_context(
    scratch_root: &std::path::Path,
    exe_dir: &std::path::Path,
) -> Result<Arc<EngineContext>, EngineError> {
    let scratch = Arc::new(Scratch::open(scratch_root)?);
    Ok(Arc::new(EngineContext {
        scratch,
        tools: super::external::ToolPaths::resolve(exe_dir),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::formats::{CollisionPolicy, Preset, Resolution};
    use crate::engine::mime::{Confidence, Detected, InputKind};

    #[test]
    fn page_stems_number_only_after_the_first() {
        assert_eq!(page_stem("report", 0), "report");
        assert_eq!(page_stem("report", 1), "report_p2");
        assert_eq!(page_stem("report", 9), "report_p10");
    }

    fn context() -> (Arc<EngineContext>, PathBuf) {
        let root = std::env::temp_dir().join(format!("filealchemy-pipe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let ctx = build_context(&root.join("temp"), &root).unwrap();
        (ctx, root)
    }

    fn request(id: u64, source: PathBuf) -> TaskRequest {
        TaskRequest {
            id,
            source,
            detected: Detected {
                kind: InputKind::Txt,
                mime: "text/plain".into(),
                confidence: Confidence::Content,
                note: None,
            },
            target: Format::Markdown,
            preset: Preset::Balanced,
            resolution: Resolution::Original,
            output_dir: source.parent().unwrap().to_path_buf(),
            collision: CollisionPolicy::AutoRename,
            purge_on_cancel: true,
        }
    }

    #[test]
    fn missing_source_reports_a_readable_failure_and_cleans_up() {
        let (ctx, root) = context();
        let scratch_root = ctx.scratch.root().to_path_buf();
        let mut engine = Engine::new(ctx, 1);

        // A source that does not exist: every engine must fail, and the failure
        // must arrive as an event rather than a panic or a silent hang.
        let req = request(7, root.join("does-not-exist.txt"));
        engine.submit(req).unwrap();
        assert_eq!(engine.active_tasks(), 1);

        let mut outcome = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(event) = engine.recv_timeout(Duration::from_millis(200)) {
                match event {
                    EngineEvent::Done { .. }
                    | EngineEvent::Cancelled { .. }
                    | EngineEvent::Failed { .. } => {
                        outcome = Some(event);
                        break;
                    }
                    _ => {}
                }
            }
        }

        // Give the worker a moment to finish its bookkeeping.
        for _ in 0..50 {
            if engine.active_tasks() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        engine.shutdown();

        match outcome {
            Some(EngineEvent::Failed { id, message }) => {
                assert_eq!(id, 7);
                assert!(!message.is_empty(), "failure must explain itself");
            }
            Some(other) => panic!("expected a failure event, got {other:?}"),
            None => panic!("engine produced no terminal event"),
        }

        // The scratch area must be empty again.
        let leftovers: Vec<_> = std::fs::read_dir(&scratch_root)
            .map(|it| it.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "scratch not scrubbed: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn place_outputs_names_multi_page_results() {
        let root = std::env::temp_dir().join(format!("filealchemy-place-{}", std::process::id()));
        let work = root.join("work");
        let out = root.join("out");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&out).unwrap();

        let pages: Vec<PathBuf> = (1..=3)
            .map(|i| {
                let p = work.join(format!("page-{i}.png"));
                std::fs::write(&p, format!("page {i}")).unwrap();
                p
            })
            .collect();

        let mut req = request(2, root.join("scan.pdf"));
        req.target = Format::Png;
        let placed = place_outputs(&req, &pages).unwrap();
        let names: Vec<String> = placed
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["scan.png", "scan_p2.png", "scan_p3.png"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
