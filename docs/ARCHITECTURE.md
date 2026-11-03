# FileAlchemy — Architecture

This document explains *why* the code looks the way it does. The specification
asks for a set of behaviours; this file records how each one is achieved and
where a deliberate engineering trade-off was made.

---

## 1. Shape of the program

```
                       ┌──────────────────────────────────────────┐
   WM_DROPFILES ──────►│  WndProc hook (platform/windows.rs)      │
   WM_COPYDATA  ──────►│  → VecDeque<Command>                      │
   hotkeys      ──────►│                                          │
                       └───────────────┬──────────────────────────┘
                                       │ (polled every 80 ms)
┌──────────────────────┐   ┌───────────▼────────────┐   ┌────────────────────┐
│ ui/appwindow.slint   │◄──│ src/app.rs             │──►│ src/engine/        │
│ custom-drawn, dark   │   │ owns *every* Slint     │   │ pipeline (workers) │
│ virtualized ListView │──►│ model; never blocks    │◄──│ crossbeam events   │
└──────────────────────┘   └────────────────────────┘   └─────────┬──────────┘
                                                                  │
                                     ┌────────────────────────────┼───────────────┐
                                     ▼                            ▼               ▼
                             image_convert.rs              media.rs / docs.rs   archive.rs
                             (in-process Rust)         (ffmpeg / pandoc /     (zip, 7z,
                                                        pdftoppm via JobObject)  tar.gz)
                                     │                            │               │
                                     └────────────► data/temp/task-* ◄────────────┘
                                                          │
                                                  atomic placement
                                                          ▼
                                                  destination file
```

* `src/app.rs` is the only module that knows about both Slint and the engine.
* `src/engine/*` is UI-agnostic and unit-testable on any host.
* `src/platform/*` isolates every Win32 call behind a small, documented API, and
  has a non-Windows fallback so `cargo test` works on CI containers.

## 2. Threading model

| Thread | Owns | Never does |
|---|---|---|
| UI thread | all Slint models, config, HWND commands | file I/O, waiting on children |
| N worker threads (`max_concurrent_tasks`, 1–16) | one conversion at a time | touch Slint types |
| WndProc (UI thread, inside the message pump) | nothing | mutates models directly — it only queues `Command`s |
| Per-child stdio readers | one pipe each | anything else |

Why a timer instead of `invoke_from_event_loop`: Slint models are `!Send`, so a
worker cannot legally push a model update. Workers therefore only send
`EngineEvent`s over a `crossbeam-channel`, and an 80 ms `Timer` on the UI thread
drains it. 80 ms is imperceptible for progress bars and it keeps the window
responsive while four ffmpeg jobs saturate the CPU.

The WndProc hook runs *inside* winit's message pump. Calling into Slint from
there would re-enter the UI while it is rendering, so the hook only appends to a
`Mutex<VecDeque<Command>>`; the same 80 ms timer consumes it. Worst-case key
latency is 80 ms, which is invisible in practice and completely removes a class
of re-entrancy bugs.

## 3. Process containment

Every child (`ffmpeg.exe`, `pandoc.exe`, `pdftoppm.exe`) is spawned with
`CREATE_NO_WINDOW` and immediately assigned to one process-wide JobObject
configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (see
`platform/windows.rs::jobobject`). Consequences:

* If FileAlchemy exits normally, crashes, or is killed from Task Manager, the
  kernel terminates every descendant. No orphaned ffmpeg eating a CPU core.
* Windows 8+ supports nested jobs, so this also works when FileAlchemy itself
  runs inside another job (CI agents, sandboxes, Windows Terminal).

Cancellation is cooperative at the top and preemptive where it counts: the
runner polls the task's `CancelToken` every 60 ms and calls `child.kill()` +
`wait()` on cancel, so a two-hour video encode dies in well under a second.

## 4. Portable isolation

`src/paths.rs` decides once, at startup:

| Condition | Data folder |
|---|---|
| `portable.dat` next to the exe **or** a `data\` folder next to the exe | `<exe_dir>\data` |
| otherwise | `%LOCALAPPDATA%\FileAlchemy` |

In portable mode the process also `set_current_dir`s to its own folder, so
relative command-line arguments resolve next to the executable rather than in
whatever directory Explorer chose. Config, scratch space and logs are all
derived from that single `data_dir`, so there is exactly one place to audit for
stray writes.

## 5. Conversion dispatch

`engine::convert` routes on the *detected input kind* and validates the target
against `mime::InputKind::targets()`. The same table drives the UI (through the
`Options` global), so the two can never disagree.

| Input kind | Module | Notes |
|---|---|---|
| PNG/JPG/WebP/BMP/TIFF/GIF/ICO/SVG/QOI | `image_convert` | fully in-process |
| AVIF (input) | `image_convert` | rejected with an explanation: decoding needs the C dav1d codec |
| MP4/MKV/MOV/AVI/WebM/MP3/WAV/FLAC/AAC/OGG/M4A | `media` | ffmpeg with explicit argv |
| PDF | `docs` | `pdftoppm` → one PNG per page |
| DOCX/EPUB/MD/TXT/HTML | `docs` | pandoc |
| CSV/JSON/RTF | `docs` | native Rust (see below) |
| ZIP/7Z/TAR/GZ/TAR.GZ | `archive` | extract to staging, then repack |

Two decisions worth defending:

**CSV and JSON never go through pandoc.** pandoc's `json` reader expects
pandoc's own document AST, not arbitrary JSON, and it has no CSV reader at all.
Routing them through it would produce silently wrong output. Instead a small
RFC 4180 parser renders real tables (Markdown / HTML / aligned plain text) and
JSON is pretty-printed inside a fenced block; DOCX/EPUB targets then hand that
*known-good Markdown* to pandoc.

**PDF is rasterise-only.** pandoc cannot read PDF, and no LaTeX engine is
bundled (it would be a 400 MB install and an online package manager away). Rather
than offering a broken "PDF → DOCX" button, the matrix offers what actually
works: PDF → PNG at 150 dpi, one file per page, named `scan.png`, `scan_p2.png`, …

## 6. Atomicity

```
destination ←── ReplaceFileW / MoveFileEx ── data/temp/task-<id>-<n>/result.png
                                              (flushed + sync_all before the move)
```

* Converters never see the destination path; they only get a scratch directory.
* `scratch::place_file` picks `name.ext` / `name_1.ext` / … for *Auto-rename*,
  and uses `ReplaceFileW` for *Overwrite* (falling back to `fs::rename`, which
  is `MOVEFILE_REPLACE_EXISTING` and therefore still atomic within a volume).
* Across volumes (portable app on a USB stick, output on `C:`), we stage a
  sibling `.<name>.partial` in the destination folder, `sync_all` it, then
  rename — so the visible filename only ever appears complete.
* Every task directory is deleted when the task ends, and `Scratch::open`
  purges leftovers at startup. Since the single-instance mutex is held by then,
  that purge cannot race another copy of the app.

## 7. Type detection

`engine/mime.rs`, in order:

1. **Magic bytes** (`infer`, first 512 bytes) plus our own signatures for
   formats some `infer` releases miss (`ftyp`→MP4, `qoif`→QOI, `ustar`→TAR…).
2. **Container introspection**, because a ZIP is not a ZIP: DOCX
   (`word/document.xml`) and EPUB (`mimetype` = `application/epub+zip`) are
   recognised from their contents; a gzip stream is decompressed 512 bytes deep
   to see whether it is really a `tar.gz`.
3. **Text sniffing** for formats with no magic number: SVG, HTML, JSON, RTF.
4. **Extension**, only as a last resort — and the UI then says
   "guessed from the file extension" so the user knows.

The evidence string (`detected from file content`, `guessed from the file
extension`, …) is shown on every queue card.

## 8. UI decisions

### Software rendering
The Slint backend is forced to `winit-software` (`SLINT_BACKEND` in `main.rs`).
No OpenGL, no Direct3D, no GPU driver to blame: identical behaviour on a 2010
laptop, in an RDP session, in a VM without 3D acceleration, and on a machine
whose driver was just uninstalled. The UI is a few hundred flat rectangles and
text runs; the software rasteriser is more than fast enough, and it removes the
single most common "my GUI app shows a black window" support ticket.

### Fixed-height rows + card faces (deviation from "expanding drawer")
The queue is a `ListView`, so only visible rows exist — a 10 000-file queue
costs the same as a 10-file one. Variable-height rows inside a virtualized list
have to be re-measured on every scroll and destabilise the viewport.

The specification asks for a "Tune drawer" that expands. Instead, `Tune` and the
format pill swap the card **face** between `Summary` / `Formats` / `Tune`, all of
which share the same 118 px height. The user gets the same content (presets,
resolution, the full target list) with zero scroll jitter, and the interaction is
one click either way.

### Tooltips
Browser-style hover popups were replaced with a **persistent hint bar** plus
`accessible-description` on every control:

* it works for keyboard and screen-reader users, not just mice;
* it can never be clipped by the list viewport or fall off the bottom of the
  screen;
* the same strings double as the accessibility descriptions, so there is exactly
  one place to keep the plain-English explanation correct.

Hovering *or* focusing any control writes its explanation into the bar.

### Custom-drawn controls
Only `ListView` is taken from `std-widgets`; buttons, chips, toggles, checkboxes,
progress bars and the settings overlay are drawn from primitives in
`ui/appwindow.slint`. That keeps the dark theme consistent, avoids fighting the
Fluent style's light palette, and means the app ships no image assets at all
(glyphs are Unicode), which is what keeps it a single portable folder.

## 9. Windows integration details

| Concern | Implementation | Why this way |
|---|---|---|
| Per-Monitor V2 DPI | `app.manifest` embedded by `build.rs` via `link.exe /MANIFEST:EMBED /MANIFESTINPUT:` | no resource compiler, no Visual Studio dependency, works with `cargo build` alone |
| Immersive dark mode | `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)` with the pre-20H1 attribute as fallback, plus matching caption/border colours on Win11 | title bar matches the app instead of a bright white strip |
| Drag & drop | `DragAcceptFiles` + `WM_DROPFILES` | the spec asks for the native path; it also gives full paths and works with shell extensions |
| Single instance | named mutex `Local\FileAlchemy_SingleInstance_Mutex` | per-session, no leftover files, kernel-cleaned on crash |
| Path forwarding | `WM_COPYDATA` with a double-NUL-separated UTF-16 list (`COPYDATA_MAGIC`) | no socket, no pipe to clean up, no firewall prompt |
| Hotkeys | handled in the WndProc | `Esc`/`Enter`/`Ctrl+O` must work even when nothing in the (software-rendered) window holds focus |
| WndProc chaining | `SetWindowLongPtrW(GWLP_WNDPROC)` + `CallWindowProcW` | 20 lines, no comctl32 subclassing dependency; winit never replaces the WndProc after creating the window |
| Completion alerts | `MessageBeep(MB_ICONASTERISK)`, `FlashWindowEx(FLASHW_ALL \| FLASHW_TIMERNOFG)` | honest, native, silent-friendly (respects the sound setting) |
| File dialogs | `GetOpenFileNameW` (multi-select) and `SHBrowseForFolderW` | no extra crates, no shell-extension surprises on locked-down machines |

## 10. Specification traceability

| Spec § | Requirement | Where |
|---|---|---|
| 1 | Rust 2021, MSVC, static CRT | `Cargo.toml`, `.cargo/config.toml`, verified by `scripts/package.ps1` |
| 1 | Slint UI, no web/Electron/.NET | `ui/appwindow.slint`, `src/app.rs` |
| 1 | Per-Monitor DPI V2 manifest, dark title bar | `app.manifest`, `build.rs`, `platform/windows.rs::set_dark_mode` |
| 1 | 100% offline, no telemetry | no HTTP dependency anywhere in the tree; `Options`/`Theme` contain no URLs |
| 1 | Artifact names | `scripts/package.ps1`, `installer/*` |
| 2 | Portable `data\` isolation | `src/paths.rs`, `src/engine/scratch.rs` |
| 3 | JobObject `KILL_ON_JOB_CLOSE` | `platform/windows.rs::jobobject` |
| 3 | Single instance + IPC | `platform/windows.rs::single_instance`, `send_paths` |
| 4 | `infer`-based MIME detection + manual fallback | `engine/mime.rs` |
| 4 | Format routing matrix | `engine/formats.rs`, `engine/mod.rs::convert` |
| 4 | Atomic placement, Auto-rename / ReplaceFileW | `engine/scratch.rs` |
| 4 | Instant cancel + scrub | `engine/external.rs`, `engine/pipeline.rs`, `app.rs::cancel_all` |
| 5 | HKCU context menu, no elevation | `src/shell.rs` |
| 5 | CLI flags | `src/main.rs` |
| 5 | `config.json` schema | `src/config.rs` |
| 5 | Completion chime + taskbar flash | `platform/windows.rs::beep / flash_taskbar` |
| 6 | 820×600 default, 720×480 min, no window scrollbars | `ui/appwindow.slint` (Window preferred/min sizes) |
| 6 | Virtualized queue, per-file target filter, hint text | `ui/appwindow.slint`, `src/app.rs` |
| 6 | Keyboard navigation | WndProc hotkeys + `FocusScope` on every control |
| 7 | Deliverables | `Cargo.toml`, `build.rs`, `ui/`, `src/*` |

## 11. Known limitations (deliberate, documented)

* **WebP output is lossless.** The `image` crate's WebP encoder is lossless-only;
  adding lossy WebP would mean compiling libwebp (a C dependency) and shipping
  it. Lossless WebP is still ~30 % smaller than PNG with alpha intact. Use AVIF
  or JPG when size is the priority.
* **No PDF → DOCX.** Requires OCR/layout reconstruction; pandoc cannot do it and
  no engine is bundled. PDF → PNG is offered instead.
* **Animated GIF → still image keeps frame 1.** For animation, use MP4/WebM.
* **SVG text** is rendered with the system font database (loaded once per
  worker thread); exotic fonts may substitute or be missing. Paths and shapes
  are always exact.
* **AVIF is output-only.** `image`'s `avif` feature enables ravif, which is an
  encoder; decoding requires `avif-native` (dav1d, a C dependency) and this
  build stays pure Rust. Selecting an `.avif` input produces a message naming
  ffmpeg as the way to decode it once.
* **RTF → text is best-effort** (control-word stripping, `\u` and `\'hh`
  decoding). It is a text extractor, not an RTF layout engine.
* **Archives:** no password-protected archives, and 7Z creation uses LZMA2 only.
* **Geometry memory** stores logical pixels and is clamped to sane bounds
  (720×480 – 3840×2160).
* **Software renderer** means no GPU acceleration — irrelevant for this UI, but
  worth knowing if it is ever embedded in a 120 Hz 4K full-screen kiosk.

## 12. Testing strategy

* **Pure logic** (format matrix, MIME tables, CSV/JSON/RTF, PDF writer bytes,
  collision naming, config validation, path resolution, argument construction,
  progress parsing, archive zip-slip defence) is covered by unit tests that run
  on any host via `cargo test`.
* **Windows behaviour** (context menu, static CRT, DPI manifest, window
  creation) is exercised by the CI workflow in
  `.github/workflows/ci.yml` on `windows-latest`.
* **Human checks** — drag & drop, cancellation, dark mode, DPI scaling — are
  listed step by step in [QA-CHECKLIST.md](QA-CHECKLIST.md).
