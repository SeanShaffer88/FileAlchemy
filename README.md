# FileAlchemy

**An offline, portable, universal file converter for Windows.**

Drop almost anything on the window, pick what it should become, press Convert. No uploads, no accounts, no telemetry, no "install this codec pack" — every conversion runs on your own machine, and the whole app is one self-contained folder you can put on a USB stick.

```
FileAlchemy/
├── FileAlchemy.exe            native x64 binary, statically linked CRT
├── bin/                       bundled conversion engines
│   ├── ffmpeg.exe             audio · video · animation
│   ├── pandoc.exe             documents & markup
│   ├── pdftoppm.exe (+dlls)   PDF page rasteriser
└── data/                      portable state (created on first run)
    ├── config.json            settings
    └── temp/                  scratch space, purged on exit
```

---

## Why it exists

Most "free online converters" are a privacy trade: your tax return, your medical scan or your unpublished manuscript goes to an anonymous server, comes back as an ad-riddled download of unknown quality, and stays on someone else's disk. FileAlchemy does the same job with the machine you already own:

* **Air-gapped.** The binary contains no HTTP client. There is no update checker, no analytics SDK, no crash reporter and no license server. Pull the network cable out and every feature still works.
* **Portable.** Ship it on a stick. With `portable.dat` (or a `data\` folder) present it writes *only* inside `.\data\` — nothing in `%APPDATA%`, `%LOCALAPPDATA%`, `%PROGRAMDATA%` or `C:\Windows\Temp`.
* **Fast.** Image and archive conversions happen in-process (Rust, no subprocess, no temp-file ping-pong); media and documents shell out to bundled static engines contained in a Windows JobObject so nothing can be orphaned.
* **Honest.** Real file types are detected from magic bytes, not extensions. A `.jpg` that is actually a PNG, a `.docx` that is really a ZIP, a `.gz` that is really a `.tar.gz` — all handled, and the UI tells you which evidence it used.

---

## What it converts

| Category | Inputs | Targets | Engine |
|---|---|---|---|
| **Images** | PNG, JPG, WebP, BMP, TIFF, GIF, ICO, SVG, AVIF, QOI | PNG, JPG, WebP, AVIF, BMP, ICO, PDF | in-process Rust (`image`, `resvg`, `ravif`) |
| **Video** | MP4, MKV, MOV, AVI, WebM | MP4 (H.264), WebM (VP9), GIF, plus audio-only MP3/WAV/FLAC/M4A | `bin/ffmpeg.exe` |
| **Audio** | MP3, WAV, FLAC, AAC, OGG, M4A | MP3, WAV, FLAC, M4A | `bin/ffmpeg.exe` |
| **Documents** | PDF, DOCX, EPUB, MD, TXT, HTML, RTF, CSV, JSON | DOCX, Markdown, HTML, TXT, EPUB, PNG (per PDF page) | `bin/pandoc.exe`, `bin/pdftoppm.exe`, native CSV/JSON/RTF paths |
| **Archives** | ZIP, 7Z, TAR, GZ, TAR.GZ | ZIP, 7Z, TAR.GZ, or extract to a folder | in-process Rust (`zip`, `sevenz-rust`, `flate2`, `tar`) |

Quality presets (`Balanced`, `Smaller file`, `High quality`, `Lossless`) and output resolution (Original / 1080p / 720p / 480p / Half) are per file. Target lists are filtered per file, so you are never offered a conversion that cannot work.

## How it behaves

* **Atomic output.** Everything is written to `data/temp/task-*`, flushed and `sync_all`'d, then moved into place. A crash or power cut leaves the destination untouched — never a truncated file pretending to be finished.
* **Collision policy.** *Auto-rename* (`holiday_1.webp`, `holiday_2.webp`, …) never destroys anything; *Overwrite* replaces atomically via `ReplaceFileW`, preserving the destination's ACLs.
* **Instant cancel.** `Esc`, the Cancel button or removing a row aborts the worker, kills the child process within ~60 ms and deletes the partial scratch file immediately.
* **Single instance.** Launching again (including from the Explorer context menu) forwards the selected files over `WM_COPYDATA` to the window that is already open.
* **Nothing leaks.** If FileAlchemy is killed from Task Manager, the JobObject's `KILL_ON_JOB_CLOSE` makes the kernel take down every ffmpeg/pandoc process with it.

## Keyboard

| Key | Action |
|---|---|
| `Ctrl+O` | Browse for files |
| `Enter` | Convert the whole queue |
| `Esc` | Cancel all running conversions (or close Settings) |
| `Delete` | Remove the selected file from the queue |
| `Tab` / `Shift+Tab` | Move through every control — chips, cards, toggles |
| `Space` | Activate the focused control |

Every control also has a plain-English explanation: hover **or** keyboard-focus it and the hint bar at the bottom of the window explains what it does and why you might care. (This replaces classic tooltips deliberately — see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#tooltips).)

## Explorer integration

```powershell
FileAlchemy.exe --install-shell-menu     # add "Convert with FileAlchemy"
FileAlchemy.exe --uninstall-shell-menu   # remove it again
```

Both write only to `HKCU\Software\Classes\*\shell\FileAlchemy`, so there is **no UAC prompt and no admin requirement**. Multi-file selections are forwarded to a single window.

## Command line

```
FileAlchemy.exe [FILES...]               launch, or append files to the running window
FileAlchemy.exe --install-shell-menu     register the context menu and exit
FileAlchemy.exe --uninstall-shell-menu   remove it and exit
FileAlchemy.exe --version | --help
```

Folders are expanded recursively (depth-limited, 5 000 files max) — dropping a folder of holiday photos just works.

## Build and package

Requires Windows 10 1903+ / Windows 11, the MSVC toolchain and Rust 1.79+.

```powershell
git clone https://github.com/SeanShaffer88/FileAlchemy.git
cd FileAlchemy
.\scripts\fetch-engines.ps1     # download + SHA256-verify ffmpeg / pandoc / poppler into bin\
cargo build --release           # target\release\FileAlchemy.exe
.\scripts\package.ps1           # Setup.exe, Setup.msi, Portable.zip, SHA256SUMS.txt
```

Full details, the static-CRT guarantees and the verification checklist are in [docs/BUILD.md](docs/BUILD.md).

## Project layout

```
ui/appwindow.slint        the entire UI (custom-drawn, dark, virtualized queue)
src/main.rs               startup, CLI, single instance, event loop
src/app.rs                UI <-> engine controller (owns every Slint model)
src/config.rs             config.json schema, validation, atomic save
src/paths.rs              portable vs installed data-folder resolution
src/shell.rs              HKCU Explorer context menu (no elevation)
src/platform/             Win32: JobObject, WndProc hooks, dialogs, notifications
src/engine/               format matrix, MIME detection, converters, worker pool
docs/                     architecture, build, QA checklist
installer/                Inno Setup .iss and WiX v4 .wxs
```

## Licence

MIT for FileAlchemy itself. The bundled engines keep their own licences (ffmpeg LGPL, pandoc GPL, poppler GPL) — they are separate executables invoked as subprocesses, and their terms are documented in [`bin/README.md`](bin/README.md).
