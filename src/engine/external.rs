//! Child-process runner for the bundled CLI engines (ffmpeg, pandoc,
//! pdftoppm).
//!
//! Responsibilities:
//! * locate the bundled executables in `bin/` (falling back to `PATH` for
//!   developers who already have them installed);
//! * spawn them with **no console window**, inside the process-wide Windows
//!   JobObject (so the OS kills them if FileAlchemy dies — spec §3);
//! * stream their output line by line so the UI can show real progress;
//! * poll a cancellation flag every 60 ms and hard-kill on Esc;
//! * keep the tail of stderr so failures read like a sentence, not a stack of
//!   `OS error` text.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crossbeam_channel::{unbounded, RecvTimeoutError};

use crate::engine::{is_cancelled, CancelToken, EngineError, Result};

/// Which bundled tool a conversion needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Ffmpeg,
    Pandoc,
    Pdftoppm,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "ffmpeg",
            Tool::Pandoc => "pandoc",
            Tool::Pdftoppm => "pdftoppm",
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "ffmpeg.exe",
            Tool::Pandoc => "pandoc.exe",
            Tool::Pdftoppm => "pdftoppm.exe",
        }
    }
}

/// Resolved locations of the bundled engines.
#[derive(Clone, Debug)]
pub struct ToolPaths {
    pub exe_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub ffmpeg: Option<PathBuf>,
    pub pandoc: Option<PathBuf>,
    pub pdftoppm: Option<PathBuf>,
}

impl ToolPaths {
    /// `exe_dir/bin/<tool>` first (portable layout), then `PATH`.
    pub fn resolve(exe_dir: &Path) -> ToolPaths {
        let bin_dir = exe_dir.join("bin");
        ToolPaths {
            exe_dir: exe_dir.to_path_buf(),
            bin_dir: bin_dir.clone(),
            ffmpeg: locate(&bin_dir, Tool::Ffmpeg),
            pandoc: locate(&bin_dir, Tool::Pandoc),
            pdftoppm: locate(&bin_dir, Tool::Pdftoppm),
        }
    }

    pub fn path(&self, tool: Tool) -> Option<&Path> {
        match tool {
            Tool::Ffmpeg => self.ffmpeg.as_deref(),
            Tool::Pandoc => self.pandoc.as_deref(),
            Tool::Pdftoppm => self.pdftoppm.as_deref(),
        }
    }

    pub fn require(&self, tool: Tool) -> Result<PathBuf> {
        self.path(tool).map(Path::to_path_buf).ok_or_else(|| {
            EngineError::ToolMissing {
                tool: tool.name(),
                path: self.bin_dir.join(tool.file_name()),
            }
        })
    }

    pub fn require_ffmpeg(&self) -> Result<PathBuf> {
        self.require(Tool::Ffmpeg)
    }

    pub fn require_pandoc(&self) -> Result<PathBuf> {
        self.require(Tool::Pandoc)
    }

    pub fn require_pdftoppm(&self) -> Result<PathBuf> {
        self.require(Tool::Pdftoppm)
    }

    /// Tools the user will notice missing, for the startup self-check.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.ffmpeg.is_none() {
            out.push("ffmpeg.exe");
        }
        if self.pandoc.is_none() {
            out.push("pandoc.exe");
        }
        if self.pdftoppm.is_none() {
            out.push("pdftoppm.exe");
        }
        out
    }
}

fn locate(bin_dir: &Path, tool: Tool) -> Option<PathBuf> {
    let bundled = bin_dir.join(tool.file_name());
    if bundled.is_file() {
        return Some(bundled);
    }
    // Developer convenience: fall back to PATH so `cargo run` works before the
    // engines have been fetched by scripts/fetch-engines.ps1.
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(tool.file_name());
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// -----------------------------------------------------------------------------
//  Running
// -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        CommandSpec {
            program: program.into(),
            args: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Arguments as one printable line, for the debug log.
    pub fn command_line(&self) -> String {
        let mut out = format!("\"{}\"", self.program.display());
        for a in &self.args {
            if a.contains(' ') {
                out.push_str(&format!(" \"{a}\""));
            } else {
                out.push(' ');
                out.push_str(a);
            }
        }
        out
    }
}

pub struct RunOutcome {
    pub code: Option<i32>,
    pub stderr: String,
}

/// Spawn, stream, and wait. `on_line` receives every line from both streams.
pub fn run_streaming<F>(
    tool: &'static str,
    spec: &CommandSpec,
    cancel: &CancelToken,
    mut on_line: F,
) -> Result<RunOutcome>
where
    F: FnMut(Stream, &str),
{
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console flash, no console attached: a GUI app spawning 4 workers
        // must never blink black windows at the user.
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let mut child = command.spawn().map_err(|e| {
        EngineError::Tool {
            tool,
            code: None,
            message: format!("could not start \"{}\": {e}", spec.program.display()),
        }
    })?;

    // Kernel-level containment: if this process dies, the OS kills the child.
    crate::platform::jobobject::assign_to_global_job(&child);

    let (tx, rx) = unbounded::<(Stream, String)>();
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, Stream::Stdout, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, Stream::Stderr, tx.clone());
    }
    drop(tx);

    let mut stderr_buf = String::new();
    let outcome = loop {
        if is_cancelled(cancel) {
            // Kill the process tree: ffmpeg spawns no children, but pandoc can
            // (via a PDF engine) so we take the whole job's worth of care here.
            let _ = child.kill();
            let _ = child.wait();
            return Err(EngineError::Cancelled);
        }

        match rx.recv_timeout(Duration::from_millis(60)) {
            Ok((stream, line)) => {
                if stream == Stream::Stderr && stderr_buf.len() < 16 * 1024 {
                    stderr_buf.push_str(&line);
                    stderr_buf.push('\n');
                }
                on_line(stream, &line);
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                break child.wait()?;
            }
        }
    };

    // Drain anything still buffered.
    while let Ok((stream, line)) = rx.try_recv() {
        if stream == Stream::Stderr && stderr_buf.len() < 16 * 1024 {
            stderr_buf.push_str(&line);
            stderr_buf.push('\n');
        }
        on_line(stream, &line);
    }

    Ok(RunOutcome {
        code: outcome.code(),
        stderr: stderr_buf,
    })
}

/// Run and collect all output, ignoring the exit code. Used for probing
/// (`ffmpeg -i`, which always exits non-zero when no output file is given).
pub fn capture(spec: &CommandSpec) -> Result<String> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }

    let child = command.spawn().map_err(|e| EngineError::Tool {
        tool: "external",
        code: None,
        message: format!("could not start \"{}\": {e}", spec.program.display()),
    })?;
    crate::platform::jobobject::assign_to_global_job(&child);

    let output = child.wait_with_output()?;
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    Ok(text)
}

fn spawn_reader<R: Read + Send + 'static>(reader: R, stream: Stream, tx: crossbeam_channel::Sender<(Stream, String)>) {
    std::thread::Builder::new()
        .name("fa-child-stdio".into())
        .spawn(move || {
            let buf = BufReader::new(reader);
            for line in buf.lines() {
                match line {
                    Ok(line) => {
                        if tx.send((stream, line)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_spaces() {
        let spec = CommandSpec::new(r"C:\Program Files\bin\ffmpeg.exe")
            .arg("-i")
            .arg(r"C:\my videos\a.mp4");
        let line = spec.command_line();
        assert!(line.contains("\"-i\""), "{line}");
        assert!(line.contains("\"C:\\my videos\\a.mp4\""), "{line}");
    }

    #[test]
    fn missing_tool_produces_a_readable_error() {
        let tools = ToolPaths {
            exe_dir: PathBuf::from("."),
            bin_dir: PathBuf::from("bin"),
            ffmpeg: None,
            pandoc: None,
            pdftoppm: None,
        };
        let err = tools.require_ffmpeg().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("ffmpeg is missing"), "{text}");
        assert!(text.contains("ffmpeg.exe"), "{text}");
        assert_eq!(tools.missing().len(), 3);
    }
}
