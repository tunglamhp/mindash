<!--
  Thanks for the pull request. A few things that make it easy to review:
  - Keep it focused. One change per PR.
  - Say what you tested, and how.
  - If it changes behaviour, a test that would fail without it.
-->

## What this changes

<!-- A sentence or two. Link the issue if there is one: Fixes #123 -->

## Why

<!-- The reasoning. If it is not the obvious approach, say why it is the right one. -->

## How it was tested

<!--
  Which suites, and anything you did by hand. If it touches SSH, remote parsing
  or the file browser, say whether tools/audit_control.py was run against a real
  host — that is the only thing that validates shell quoting.
-->

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo fmt --all -- --check`
- [ ] `python tools/audit_rust.py`
- [ ] `python tools/audit_auth.py`
- [ ] `node tools/verify_rust_ui.mjs` and `node tools/verify_control_ui.mjs`
- [ ] `python tools/audit_control.py` (only if SSH, parsing or files changed)

## Checklist

- [ ] A test covers the change, or the change is not testable and I have said why
- [ ] No new runtime dependency — no database, no daemon, nothing to install
- [ ] No shell interpolation: commands are argv vectors, remote scripts are encoded
- [ ] Device routes still take an id, never a host
- [ ] New actions are enum variants, not command strings
- [ ] Documentation updated if behaviour changed

## Screenshots

<!-- For UI changes. Before and after if it is a redesign. -->
