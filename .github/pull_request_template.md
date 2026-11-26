## Summary
Describe what changed and why.

## Type of Change
- [ ] Bug fix
- [ ] New feature
- [ ] Security hardening / FFI safety
- [ ] Performance / latency improvement
- [ ] Documentation / CI / packaging

## Verification
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace` (note any GPU-dependent headless skips on non-GPU runners)
- [ ] [`CHANGELOG.md`](CHANGELOG.md) `[Unreleased]` updated (if user-visible or architectural)
- [ ] No secrets, tokens, `consoles.json`, or machine-specific paths committed
