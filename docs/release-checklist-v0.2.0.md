# Deploy & Release Checklist: `gnsh-play` `v0.2.0`

**Target Release:** `v0.2.0` (`dev` merged into `main`) | **Platform:** Windows x64 (`x86_64-pc-windows-gnu`)

## Pre-Release Verification

- [ ] **Formatting:** `cargo fmt --all -- --check` exits `0`
- [ ] **Linter:** `cargo clippy --workspace --all-targets -- -D warnings` exits `0` (0 warnings)
- [ ] **Workspace Unit & Non-Vulkan GPU Tests:**
  - `cargo test -p gnsh-app -p gnsh-proto -p gnsh-input -p gnsh-chiaki-sys` passes 100%
  - `cargo test -p gnsh-media --lib` passes (D3D11/D3D12 shader + screenshot + annex-b + recorder + `libplacebo` egui GLSL pass; document status of AMD driver headless `headless_vulkan_video_zero_copy_decodes_and_renders_nonblack_pixels` in release notes if still driver-blocked)
- [ ] **Security & Hygiene Scan:**
  - Re-run tracked + untracked file scan for tokens, private keys, `consoles.json`, `settings.json`, `*.log`, and local user home paths (0 hits)
  - Verify `LICENSE` (`AGPL-3.0-only`), `THIRD_PARTY_LICENSES.md`, `SECURITY.md`, `CONTRIBUTING.md`, `.github/`, and `CHANGELOG.md` are up to date
  - Run `cargo audit` (or CI `rustsec/audit-check`) on `Cargo.lock` with 0 unmitigated critical/high advisories
- [ ] **Version Bump (when ready to cut `v0.2.0`):**
  - Update `[workspace.package].version` in `Cargo.toml` to `"0.2.0"`
  - Update `FILEVERSION`, `PRODUCTVERSION`, `FileVersion`, and `ProductVersion` in `crates/gnsh-app/assets/gnsh-play.rc` to `0.2.0`
  - Move `CHANGELOG.md` `[Unreleased]` entries under `## [0.2.0] - YYYY-MM-DD` and refresh comparison links
  - Clean `git status` and review `git diff main..dev`

## Build & Packaging

- [ ] Ensure no running `gnsh-play.exe` process locks `dist\`
- [ ] Stage standalone portable bundle + checksum archive:
  ```powershell
  powershell -ExecutionPolicy Bypass -File .\scripts\stage-dist.ps1 -Zip
  ```
- [ ] Verify `dist-packages\gnsh-play-v0.2.0-win64.zip` and `dist-packages\gnsh-play-v0.2.0-win64.zip.sha256` match (`Get-FileHash`)
- [ ] Verify CLI entry points against `.\dist\gnsh-play.exe`:
  - `.\dist\gnsh-play.exe --help`
  - `.\dist\gnsh-play.exe --discover`
  - `.\dist\gnsh-play.exe --list`

## Manual Hardware Smoke Tests (Isolated Scratch `%APPDATA%` + Live PS5)

- [ ] **Direct3D 11 (`"renderer_backend": "D3D11"`):** Home UI renders cleanly (`#202734 / #36415A / #B7D894 / #E0F8C7`), stream connects, `D3D11VA` badge shown, `CAS` + `FSR 1.0` + `Deband` + `Vibrance` / `Contrast` / `Gamma` adjust live, HUD shows `{w}x{h}` and hover `Frame:` / `Image:`
- [ ] **Direct3D 12 (`"renderer_backend": "D3D12"`):** Stream connects (`D3D12VA` or automatic `D3D11VA` transfer fallback), non-black output verified, `Deband` + grading + `CAS`/`FSR 1.0` toggle cleanly
- [ ] **Vulkan (`"renderer_backend": "Vulkan"`):** Home UI + `Vulkan (libplacebo)` stream render cleanly (`VulkanVA` or fallback), `Deband` + `CAS` + grading toggle cleanly, HDR10 passthrough skips SDR grading
- [ ] **HDR10 -> SDR & PS5 OSK:** `HEVC HDR10` tone-maps on SDR monitor with live `HDR->SDR Exposure` slider; OSK modal opens, sends UTF-8 text, and `Esc` closes without dropping stream
- [ ] **Capture & Input:** `F12` PNG screenshot (both SDR and HDR->SDR), `F9` raw-remux `.mkv` record, `F10` Replay Buffer save clip, DualSense haptics + adaptive triggers + `L1+R1+Options` HUD chord, `F8` KB+M mouse capture

## GitHub Release & Post-Release (Maintainer Executed Manually)

- [ ] Merge `dev` -> `main`, create annotated tag `git tag -a v0.2.0 -m "gnsh-play v0.2.0"`, and push `main` + tag
- [ ] Wait for CI workflows (`.github/workflows/ci.yml`) to pass green on `main`
- [ ] Draft and publish GitHub Release `v0.2.0` attaching `gnsh-play-v0.2.0-win64.zip` + `.sha256` and copying notes from `CHANGELOG.md`
- [ ] Fresh-unzip cold test of `gnsh-play-v0.2.0-win64.zip` in a clean directory without MSYS2 on `PATH`

## Rollback & Hotfix Triggers

Rollback or ship an immediate `v0.2.1` hotfix (and pull affected release artifact) if any of the following occur:
- Startup crash or blank window on any supported Windows 10/11 GPU backend (`D3D11` fallback failure)
- Regression in DPAPI storage (`consoles.json` load failure losing saved PS5 pairings)
- Audio/video deadlock, >5% frame drops on 1080p60 LAN, or broken DualSense/KB+M input handling
- Security vulnerability or credential/PII leak in distributed artifacts or logs
