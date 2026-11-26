# Security Policy

## Supported Versions

Security fixes are applied to the active development branch (`dev`) and included in the latest release on `main`:

| Version | Supported |
| :--- | :--- |
| `0.2.x` | Yes |
| `0.1.x` (`main`) | Security backports / upgrade to latest |

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Report suspected vulnerabilities privately via GitHub Security Advisories:

- <https://github.com/Gnesh97/gnsh-play/security/advisories/new>

Include as much of the following as possible:

- Affected component or file path (e.g., `crates/gnsh-proto`, `crates/gnsh-media`, `crates/gnsh-app`)
- Windows build, active renderer (`Direct3D 11`, `Direct3D 12`, or `Vulkan`), and steps to reproduce
- Proof-of-concept input or crash trace (please **never** attach `%APPDATA%\gnsh-play\consoles.json` or PSN tokens)

We aim to acknowledge reports within **72 hours** and provide an initial triage assessment or remediation plan within **7 days**.

## Security Boundaries in `gnsh-play`

- **Credential Storage:** Console `psn_account_id`, `regist_key`, and `rp_key` secrets are encrypted per Windows user via Win32 DPAPI (`CryptProtectData`) inside `%APPDATA%\gnsh-play\consoles.json` (`1 MiB` config file read limit, redacted `Debug` output).
- **Network & URL Validation:** Console LAN/WAN hosts are validated against `validate_console_host` (rejecting userinfo `@`, shell delimiters, `0.0.0.0`, multicast, broadcast, and `169.254.169.254`). PSN OAuth uses HTTPS-only `ureq` + `rustls` with capped responses (`64 KiB`), and `ShellExecuteW` only opens validated `https://` URLs.
- **Filesystem & Capture Paths:** Custom capture directories are validated to stay on local drive roots without UNC shares (`\\server\share`), Win32 device prefixes (`\\?\`), control characters, or `..` traversal segments; recordings and screenshots write via temporary files and atomic rename.
- **Native FFI Boundaries:** `gnsh-media`, `gnsh-input`, and `gnsh-proto` interface with FFmpeg, `libchiaki`, SDL3, `libplacebo`, and Direct3D/Vulkan with explicit packet/frame dimension caps, checked plane strides, and UTF-8 / control-character sanitization.
