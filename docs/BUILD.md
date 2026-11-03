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
* **Expected first-run friction:** third-party API drift in the pinned
  dependency versions (most likely `ravif::Img` / `SevenZWriter::push_source`,
  which are isolated to single functions and marked with comments). Everything
  else uses stable, long-lived APIs.
* **Next step on a Windows machine:**

  ```powershell
  cargo fmt
  cargo build
  cargo test
  cargo clippy --all-targets
  ```

  Fix anything that surfaces, then run [QA-CHECKLIST.md](QA-CHECKLIST.md) before
  tagging a release.
