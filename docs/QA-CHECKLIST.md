# Manual QA checklist

Automated tests cover the pure logic; these are the behaviours that need a real
Windows desktop. Run the list top to bottom before tagging a release. Each item
is phrased so a pass/fail is unambiguous.

## Setup

```powershell
.\scripts\fetch-engines.ps1
.\scripts\package.ps1
# unzip dist\FileAlchemy-v*-x64-Portable.zip to a USB stick if you can
```

---

## 1. Startup & portability

- [ ] First launch creates `data\config.json` **and** `data\temp\`, and nothing
      appears in `%APPDATA%`, `%LOCALAPPDATA%`, `%PROGRAMDATA%` or `C:\Windows\Temp`
      (watch with Process Monitor, filtered to the FileAlchemy.exe process).
- [ ] Deleting `data\config.json` and relaunching restores the documented defaults.
- [ ] Writing garbage into `config.json` shows a warning on the console (when
      launched from a terminal) and still starts with defaults.
- [ ] An installed copy (Setup.exe / MSI) writes to `%LOCALAPPDATA%\FileAlchemy`
      instead, and never to the program folder.
- [ ] Moving the portable folder to a path with spaces and non-ASCII characters
      (`D:\Мои файлы\FileAlchemy\`) still works.
- [ ] Run from a folder on a USB stick whose drive letter changes: settings
      still load.

## 2. Drag & drop

- [ ] Drag 5 mixed files from Explorer onto the window → 5 cards appear, each
      with the right category badge and target format.
- [ ] Drag a folder → its supported files are added recursively.
- [ ] Drag a `.jpg` that is really a PNG → badge says PNG and the detail line
      says "detected from file content".
- [ ] Drag a `.docx` → detected as DOCX (not just "ZIP"), thanks to the
      container inspection.
- [ ] Drag an unsupported file (e.g. `.exe`) → it appears as a failed row with a
      readable reason, and the app keeps working.
- [ ] Drag files onto the window while a conversion is running → they are
      appended, not lost.
- [ ] Drop onto the window while it is minimised → it restores and the files land.

## 3. Single instance

- [ ] With the app open, run `FileAlchemy.exe "C:\path\a.png"` from cmd → the
      running window comes forward with the file queued, and no second window
      appears.
- [ ] With the app open, select 3 files in Explorer → right-click → *Convert
      with FileAlchemy* → all three are queued in one window.
- [ ] Close the app, then launch twice quickly from Explorer → exactly one
      window survives.

## 4. Conversions (spot-check each category, then the matrix edges)

- [ ] PNG (with transparency) → JPG: transparency becomes white, no black halo.
- [ ] PNG → WebP → PNG round-trip keeps the alpha channel.
- [ ] JPG → AVIF: file is smaller and opens in Edge/Explorer thumbnails.
- [ ] AVIF → PNG: card shows a clear "AVIF is an output-only format here"
      message (decoding would need a C codec) instead of a generic decode error.
- [ ] SVG → PNG renders shapes; text (if the SVG has any) is legible.
- [ ] PNG → PDF: opens in Edge, one page, correct aspect ratio, no blurring
      beyond the source pixels.
- [ ] 1920×1080 PNG → 720p: height is exactly 720, aspect ratio preserved.
- [ ] MP4 (2+ min) → WebM: progress bar advances smoothly and finishes at 100 %.
- [ ] MP4 → GIF: animation loops, colours dithered (no banding artefacts).
- [ ] MP4 → MP3: audio-only output, correct duration.
- [ ] WAV → FLAC → WAV: same duration, no audible difference.
- [ ] DOCX → Markdown → DOCX round-trip keeps headings, lists and tables.
- [ ] CSV → HTML: opens as a real table in a browser.
- [ ] JSON → TXT: pretty-printed, valid JSON.
- [ ] Scanned PDF (3 pages) → PNG: three files named `scan.png`, `scan_p2.png`,
      `scan_p3.png`.
- [ ] ZIP → 7Z → TAR.GZ → ZIP: directory structure preserved.
- [ ] ZIP → *Extract to folder*: files unpacked, no `../` escape from a hostile
      archive (use a test archive with a `../evil.txt` entry).

## 5. Output handling

- [ ] Default destination writes next to the source, and the source file is
      untouched (compare timestamps and hashes).
- [ ] Converting the same file twice with *Auto-rename* produces `name.webp`,
      `name_1.webp`, `name_2.webp`.
- [ ] *Overwrite* replaces the existing file atomically; no `*.tmp` remains.
- [ ] *Custom folder*: pick a folder, convert, verify files land there; the
      choice survives a restart.
- [ ] Convert to a read-only destination folder → the row shows a clear error
      (permission denied), not a silent failure.
- [ ] While a conversion runs, `data\temp\task-*` exists; after it completes,
      `data\temp` is empty.

## 6. Cancellation & resilience

- [ ] Start a long video conversion, press `Esc` → progress stops within ~1 s,
      the row says Cancelled, and Task Manager shows no ffmpeg.exe left.
- [ ] Kill FileAlchemy from Task Manager mid-conversion → ffmpeg.exe disappears
      from Task Manager within a second (JobObject did its job).
- [ ] After a cancelled conversion, `data\temp` contains no files.
- [ ] Cancel, then convert the same file again → it succeeds (no stale handle).

## 7. Keyboard & accessibility

- [ ] `Ctrl+O` opens the file dialog even when nothing is focused.
- [ ] `Tab` reaches every control: header buttons, cards, chips, footer, toggles.
- [ ] `Space` activates the focused chip/button/toggle.
- [ ] `Enter` converts; `Esc` cancels; `Delete` removes the selected row.
- [ ] `Ctrl+,` opens Settings; `Esc` closes it and does **not** cancel the queue.
- [ ] Narrator reads the card name, category, target format and status.
- [ ] Hovering and focusing each control updates the hint bar with a plain-English
      explanation (no jargon, no "CRF" without explanation).

## 8. Shell integration

- [ ] `--install-shell-menu` adds the entry with the correct label and icon, and
      no UAC prompt appears.
- [ ] Right-click on a single file works; right-click on a 3-file selection works.
- [ ] `--uninstall-shell-menu` removes the key (verify in `regedit`).
- [ ] Toggling it in Settings applies immediately (no restart) and survives a
      restart.

## 9. Notifications

- [ ] Queue finishes with sound enabled → chime plays exactly once.
- [ ] Queue finishes with sound disabled → silence.
- [ ] Queue finishes while the window is in the background → taskbar button
      flashes (and stops when the window is activated).
- [ ] Cancelling the queue does **not** chime or flash.

## 10. Visual / DPI

- [ ] Window opens at 820×600 and cannot be resized below 720×480.
- [ ] The window has no outer scrollbars at any size; only the queue list scrolls.
- [ ] At 150 % and 200 % display scaling, text is crisp and nothing is clipped.
- [ ] Moving the window between a 100 % and a 200 % monitor rescales correctly.
- [ ] Windows in dark mode: title bar is dark and matches the app chrome.
- [ ] Windows in light mode: the app stays dark and the title bar is still
      readable (caption colour follows the theme).
- [ ] Toggling high-contrast mode does not make text unreadable.

## 11. Missing engines

- [ ] Rename `bin\ffmpeg.exe`, then try to convert an MP4: the row fails with
      "ffmpeg is missing. FileAlchemy expects it at …".
- [ ] Rename `bin\pandoc.exe`, convert Markdown → DOCX: same kind of message.
- [ ] Image → PNG still works with every engine removed (in-process path).
- [ ] On launch with engines missing, the console lists them; the GUI still starts.

## 12. Packaging

- [ ] `SHA256SUMS.txt` matches `certutil -hashfile <artifact> SHA256`.
- [ ] Setup.exe installs per-user, no UAC, creates a Start-menu shortcut, and
      the optional Explorer task works.
- [ ] Uninstall removes the program files, the shortcut and the context menu.
- [ ] MSI installs and upgrades in place (install v1.0.0, then v1.0.1).
- [ ] Portable zip on a machine without Rust, without the VC++ runtime and
      without any codec pack runs correctly.
