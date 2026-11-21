# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added (2026-11-22)
- Initialized repository structure with `README.md`, `.gitignore`, and `CHANGELOG.md`.
- Configured private GitHub remote (`Gnesh97/gnsh-play`).
- Added initial architecture overview ([`docs/architecture.md`](docs/architecture.md)) and updated [`README.md`](README.md) with project navigation links.
- Scaffolded the Rust Cargo workspace (`Cargo.toml`, `.gitignore`), added `streetpea/chiaki-ng` as `third_party/chiaki-ng`, built static `libchiaki.a` via MSYS2 MINGW64, and implemented `crates/gnsh-chiaki-sys` FFI bindings verified with a passing `chiaki_lib_init` smoke test.
