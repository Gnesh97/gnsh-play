# Third-Party Licenses & Attribution

`gnsh-play` is licensed under the GNU Affero General Public License v3.0 (`AGPL-3.0-only`) with the OpenSSL linking exception (see [`LICENSE`](LICENSE)). Because `gnsh-play` statically links `streetpea/chiaki-ng` and dynamically links native MSYS2/MinGW64 libraries packaged by [`scripts/stage-dist.ps1`](scripts/stage-dist.ps1), the following upstream components and licenses apply.

## Native & Bundled Dependencies

| Component | License | Usage in `gnsh-play` | Upstream |
| :--- | :--- | :--- | :--- |
| `chiaki-ng` (`libchiaki`) | `AGPL-3.0-only` (+ OpenSSL exception) | Static C core for PS5 discovery, registration, session/FEC, audio, haptics, OSK | `third_party/chiaki-ng` |
| `FFmpeg` (`avcodec`, `avformat`, `avutil`, `swscale`) | `GPL-3.0-or-later` / `LGPL-2.1-or-later` (per MSYS2 `mingw-w64-x86_64-ffmpeg` build) | `D3D11VA` / `D3D12VA` / `Vulkan Video` hardware & software video decode, PNG/MKV capture | FFmpeg Project |
| `SDL3` | `Zlib` | Windowing, Win32/Vulkan surface creation, DualSense gamepad/haptics/LEDs, audio playback & mic capture | libsdl-org |
| `libplacebo` | `LGPL-2.1-or-later` | Opt-in Vulkan video presentation, debanding, and custom `PRE_OUTPUT` CAS/grade shader hook | VideoLAN / libplacebo |
| `Vulkan-Loader` (`vulkan-1`) | `Apache-2.0` | Vulkan ICD loader | Khronos Group |
| `OpenSSL` (`libcrypto`) | `Apache-2.0` | TLS / cryptography required by `libchiaki` & `curl` | OpenSSL Project |
| `Opus` | `BSD-3-Clause` | Audio decode & microphone uplink encoding | Xiph.Org Foundation |
| `json-c` | `MIT` | JSON parsing inside `libchiaki` | json-c |
| `libevent` | `BSD-3-Clause` | Event loop required by `libchiaki` | libevent |
| `miniupnpc` | `BSD-3-Clause` | UPnP helpers linked by `libchiaki` | MiniUPnP |
| `libcurl` | `curl` (MIT/X-derivate) | HTTP client linked by `libchiaki` | curl |
| MinGW-w64 / GCC Runtime DLLs | `GCC Runtime Library Exception` / `ZPL-2.1` | Redistributed runtime DLLs staged into `dist/` by `scripts/stage-dist.ps1` | MinGW-w64 / GNU |

## Rust Workspace Crates (`Cargo.lock`)

Direct and key transitive Rust dependencies declared in [`Cargo.toml`](Cargo.toml) and pinned in [`Cargo.lock`](Cargo.lock):

- `egui` (`0.37`) — `MIT OR Apache-2.0`
- `windows` (`0.62`) — `MIT OR Apache-2.0`
- `ureq` (`3.5`) / `rustls` (`0.23`) / `ring` (`0.17`) — `MIT OR Apache-2.0` / `Apache-2.0 AND ISC`
- `serde` (`1`) / `serde_json` (`1`) / `anyhow` (`1`) / `thiserror` (`2`) / `base64` (`0.23`) / `crossbeam-channel` (`0.5`) / `tracing` (`0.1`) — `MIT OR Apache-2.0` (with `BSD-3-Clause` / `Unicode-3.0` transitives per upstream)
- `bindgen` (`0.73`, build-time only) — `BSD-3-Clause`

Running `cargo license` or inspecting `Cargo.lock` reproduces the full Rust crate license inventory.
