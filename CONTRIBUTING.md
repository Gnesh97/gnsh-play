# Contributing to `gnsh-play`

Thank you for helping improve `gnsh-play`. By contributing, you agree that your contributions are licensed under the project license ([`AGPL-3.0-only`](LICENSE)).

## Development Environment (Windows x64)

1. **Rust toolchain:** Install `stable-x86_64-pc-windows-gnu` via `rustup` (`rustfmt` and `clippy` components enabled).
2. **MSYS2 MINGW64:** Install MSYS2 (`C:\msys64\mingw64`) and the native toolchain + dependencies:

   ```bash
   pacman -S --needed \
     mingw-w64-x86_64-gcc \
     mingw-w64-x86_64-cmake \
     mingw-w64-x86_64-ninja \
     mingw-w64-x86_64-clang \
     mingw-w64-x86_64-pkgconf \
     mingw-w64-x86_64-protobuf \
     mingw-w64-x86_64-python \
     mingw-w64-x86_64-python-protobuf \
     mingw-w64-x86_64-openssl \
     mingw-w64-x86_64-opus \
     mingw-w64-x86_64-json-c \
     mingw-w64-x86_64-miniupnpc \
     mingw-w64-x86_64-libevent \
     mingw-w64-x86_64-curl \
     mingw-w64-x86_64-ffmpeg \
     mingw-w64-x86_64-sdl3 \
     mingw-w64-x86_64-vulkan-headers \
     mingw-w64-x86_64-vulkan-loader \
     mingw-w64-x86_64-libplacebo
   ```

3. **Initialize submodule & build `third_party/chiaki-ng`:**

   Do **not** modify `third_party/chiaki-ng` unless specifically coordinating an upstream submodule change.

   ```powershell
   git submodule update --init --recursive
   $env:PATH = "C:\msys64\mingw64\bin;C:\msys64\usr\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"
   cmake -S third_party/chiaki-ng -B third_party/build/chiaki-ng -G Ninja `
     -DCMAKE_BUILD_TYPE=Release `
     -DCHIAKI_ENABLE_TESTS=OFF `
     -DCHIAKI_ENABLE_CLI=OFF `
     -DCHIAKI_ENABLE_GUI=OFF `
     -DCHIAKI_ENABLE_ANDROID=OFF `
     -DCHIAKI_ENABLE_STEAM_SHORTCUT=OFF `
     -DCHIAKI_ENABLE_FFMPEG_DECODER=OFF `
     -DCHIAKI_USE_SYSTEM_CURL=ON
   cmake --build third_party/build/chiaki-ng
   ```

## Checks Before Submitting a PR

Run all checks in a MINGW64-enabled PowerShell shell:

```powershell
$env:PATH = "C:\msys64\mingw64\bin;C:\msys64\usr\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:LIBCLANG_PATH = "C:\msys64\mingw64\bin"

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- Follow **Conventional Commits** (`feat(media): ...`, `fix(app): ...`, `security: ...`, `docs: ...`).
- Keep `[Unreleased]` in [`CHANGELOG.md`](CHANGELOG.md) up to date.
- Never commit secrets, personal `%APPDATA%\gnsh-play\` configs (`consoles.json`, `settings.json`, `gnsh-play.log`), or local machine paths.
