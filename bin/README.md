# bin/ — bundled conversion engines

These executables are **build inputs**, fetched by
`scripts/fetch-engines.ps1` and pinned by SHA256 in `engines.lock.json`. They
are never downloaded at runtime: FileAlchemy makes no network requests at all.

| File | Purpose | Licence |
|---|---|---|
| `ffmpeg.exe` | audio, video and animated-media transcoding | LGPL-2.1-or-later (lgpl build: no GPL-only encoders) |
| `pandoc.exe` | document AST conversions (DOCX, EPUB, Markdown, HTML, TXT) | GPL-2.0-or-later |
| `pdftoppm.exe` + Poppler DLLs | PDF page rasteriser | GPL-2.0-or-later |

## Why separate executables rather than linked libraries

* **Licensing stays simple.** FileAlchemy is MIT. Calling GPL/LGPL programs as
  subprocesses keeps those programs at arm's length; statically linking them
  would drag their terms into the application.
* **Crash isolation.** A malformed video that crashes ffmpeg cannot take the UI
  with it.
* **Containment.** Every child runs inside a Windows JobObject with
  `KILL_ON_JOB_CLOSE`, so nothing survives the app.
* **Portability.** Swapping in a patched or differently-licensed build is
  dropping a file in this folder.

## Which builds to pin

* **ffmpeg** — the `lgpl` Windows x64 build (not `gpl`, not `full`) keeps
  redistribution obligations to attribution and source availability. It still
  includes `libx264`, `libvpx`, `libopus` and `libmp3lame`, which covers every
  target in the format matrix.
* **pandoc** — the single-file Windows x64 release; no external dependencies.
* **poppler** — `pdftoppm.exe` needs its sibling `freetype`, `jpeg`, `png`,
  `tiff`, `zlib` DLLs. `fetch-engines.ps1` copies the whole
  `Library\bin` folder for you (`copy_siblings: true` in the lock file).

## Updating an engine

```powershell
# 1. edit url/archive/member in engines.lock.json (leave sha256 empty)
# 2. fetch and record the observed hash
.\scripts\fetch-engines.ps1 -Force -Update
# 3. review the diff, run the QA checklist, commit both files
```

The script refuses to install anything whose hash does not match, so a
compromised or silently re-uploaded release cannot slip into a build.
