# Building FileAlchemy

Target: **Windows 11 x64**, compatible with Windows 10 x64 1903+.

## 1. Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Windows | 10 1903+ / 11 | SDK not required to build |
| Rust | 1.79+ (`stable-x86_64-pc-windows-msvc`) | <https://rustup.rs> |
| Visual Studio Build Tools | 2019/2022, "Desktop development with C++" | provides `link.exe` |
| PowerShell | 5.1+ | for the helper scripts |
| Inno Setup 6 | optional | produces `Setup.exe` (`choco install innosetup`) |
| WiX v4 | optional | produces `Setup.msi` (`dotnet tool install --global wix`) |

There is **no network access at runtime** and no package manager on the target
machine. Everything the app needs is either compiled in or sits in `bin\`.

## 2. First build

```powershell
.\scripts\fetch-engines.ps1     # ffmpeg.exe, pandoc.exe, pdftoppm.exe(+DLLs) -> bin\
cargo build --release
.\target\release\FileAlchemy.exe
```

`fetch-engines.ps1` verifies each archive against `engines.lock.json` before
extracting anything. A missing hash blocks the install on purpose:

```
Refusing to install pdftoppm.exe without a recorded hash.
Re-run with -Update if <hash> is the release you intend to pin.
```

Run `.\scripts\fetch-engines.ps1 -Update` once per version bump, review the
diff in `engines.lock.json`, and commit it. That is the only moment a
third-party binary is allowed in.

> `cargo build` generates `Cargo.lock`. **Commit it.** Releases should be built
> from a lock file; `scripts/package.ps1` passes `--locked` when one is present.

## 3. What the build guarantees

`build.rs` does two things beyond compiling the Slint UI:

1. `slint_build::compile_with_config("ui/appwindow.slint")` — the UI is compiled
   into Rust at build time (no runtime `.slint` parsing, no assets to lose).
2. On `x86_64-pc-windows-msvc` it appends `/MANIFEST:EMBED`,
   `/MANIFESTINPUT:app.manifest` and `/MANIFESTUAC:NO` to the linker. That is
   what makes the executable Per-Monitor DPI V2 aware and `asInvoker` (never
   elevates) without a resource-compiler step.

`.cargo/config.toml` sets `target-feature=+crt-static` for the MSVC targets, so
the binary never depends on `VCRUNTIME140.dll` / `MSVCP140.dll`. This matters
for a portable app: there is no VC++ redistributable to install on the target.

`scripts/package.ps1` **verifies** both properties instead of trusting them:

* byte-scans the executable for `VCRUNTIME140.dll`, `MSVCP140.dll` and
  `api-ms-win-crt-runtime` imports → build fails if present;
* scans for `PerMonitorV2` in the embedded (UTF-16) manifest → warns if absent.

The CI workflow performs the same checks on every push.

## 4. Tests

```powershell
cargo test            # 60+ unit tests: formats, config, mime, CSV/JSON/RTF,
                      # PDF bytes, placement/collision, paths, ffmpeg argv,
                      # progress parsing, zip-slip defence
cargo clippy --all-targets
```

The tests are host-agnostic (they only use the engine, config and platform
fallbacks), so they also run on Linux/macOS CI containers — useful because the
GUI itself needs Windows.

## 5. Packaging

```powershell
.\scripts\package.ps1
```

Produces, in `dist\`:

| File | Built by |
|---|---|
| `FileAlchemy-v{VERSION}-x64-Setup.exe` | Inno Setup, `installer/filealchemy.iss` |
| `FileAlchemy-v{VERSION}-x64-Setup.msi` | WiX v4, `installer/filealchemy.wxs` |
| `FileAlchemy-v{VERSION}-x64-Portable.zip` | `Compress-Archive` of `dist\portable` |
| `SHA256SUMS.txt` | `Get-FileHash` over every artifact |

Both installers are **per-user**: they land in
`%LOCALAPPDATA%\Programs\FileAlchemy`, never prompt for UAC, and never write to
`HKLM`. The portable zip ships `portable.dat` plus an empty `data\temp\`, so it
is unambiguously portable the moment it is unzipped.

Uninstalling keeps `%LOCALAPPDATA%\FileAlchemy\config.json` (it is user state,
not build output); delete that folder to reset every setting.

## 6. Release

```powershell
git tag v1.0.0
git push origin v1.0.0
```

`.github/workflows/release.yml` then verifies the engine hashes, runs the tests,
builds all three artifacts plus `SHA256SUMS.txt`, and publishes them to a GitHub
release. No artifact is uploaded if an engine hash does not match.

## 7. Verification status of this snapshot

Honest accounting, because a plan is not a build:

* **Reviewed by hand:** every module in `src/`, `ui/appwindow.slint`, both
  installer scripts, `build.rs`, `.cargo/config.toml`, the workflows, and the
  packaging script.
* **Not yet executed:** `cargo build`, `cargo test`, `cargo clippy`, Inno Setup
  and WiX. The source snapshot was produced in an environment without a Rust
  toolchain and without access to crates.io, so no compilation has been claimed
  or implied.
* **What CI has actually proven so far:** dependency resolution and
  compilation up to this crate, and one real diagnostic
  (`Cannot access id 'clear-lbl'`), which is fixed. The steps after the Slint
  compile — unit tests, the release build, the static-CRT scan, the CLI smoke
  test and the GUI launch — have never run yet, because the job stops at the
  first failing step.
* **If a CI job fails and the log URL is unreachable** (it is signed blob
  storage, and an egress policy can block it): every compile and test failure
  is re-emitted as a check-run annotation, which is readable from anywhere with
  `gh api repos/OWNER/REPO/check-runs/JOB_ID/annotations`. That mechanism is
  deliberate.
* **Slint id-scope rule learned from CI:** an id declared inside an `if` or
  `for` body cannot be referenced from that body's *own* properties; reference
  it only from descendants. Slint's
  `internal/compiler/tests/syntax/lookup/for_lookup.slint` documents the same
  boundary. Sibling forward references are fine — Slint's own fluent `Button`
  binds `min-width` against a layout declared further down.
* **Cross-checked against upstream sources** (not compiled, but read off the
  actual tagged sources rather than memory): `windows-sys` 0.59 feature names
  (all 15 exist), Slint 1.8 cargo features (`backend-winit`,
  `renderer-software`, `software-renderer-systemfonts`, `compat-1-2`,
  `accessibility`), `resvg` 0.44's `render(&Tree, Transform, &mut PixmapMut)`
  and `Options::fontdb_mut()`, `zip` 2.2's `bzip2`/`deflate`/`time` features and
  `SimpleFileOptions`, and ravif's builder (`with_quality`/`with_speed`/
  `with_alpha_quality` return `Self`; `encode_rgba` takes an `imgref` view).
* **API drift that has already been fixed** in this snapshot: `image`'s `avif`
  feature is encoder-only (so AVIF input is now rejected with an explanation
  instead of failing to decode), `SevenZWriter::push_source` became
  `push_archive_entry` with a hand-built entry, and `ravif::Img` became
  `imgref::ImgVec`.
* **Expected first-run friction:** whatever is left is most likely a type
  mismatch inside one converter function; each third-party API call is confined
  to a single function with a comment saying so.
* **Next step on a Windows machine:**

  ```powershell
  cargo fmt
  cargo build
  cargo test
  cargo clippy --all-targets
  ```

  Fix anything that surfaces, then run [QA-CHECKLIST.md](QA-CHECKLIST.md) before
  tagging a release.
