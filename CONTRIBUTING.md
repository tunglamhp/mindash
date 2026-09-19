# Contributing to MinDash

Thanks for wanting to help. This is a small project with strong opinions about a
few things, so a short read here will save us both time.

## Getting set up

You need **Rust 1.80+** and **Python 3** (the build script is Python; there is no
Python in the product).

```bash
git clone https://github.com/tunglamhp/mindash.git
cd mindash
python tools/build_rust.py --run     # build both artefacts, serve on :5050
```

Open <http://127.0.0.1:5050>. The first run creates `rust/devdata/`.

## Before you open a pull request

Run these. All four pass on `main`, and CI runs the same set:

```bash
cargo test --workspace                # unit tests
cargo clippy --workspace -- -D warnings
python tools/audit_rust.py            # config, widgets, auth, hardening, branding
node tools/verify_rust_ui.mjs         # dashboard, in a real browser
node tools/verify_control_ui.mjs      # devices, files, containers, in a browser
```

The browser suites need a server running on `:5050` and Chrome installed. They
start their own fixture and reset it, so they are safe to re-run.

If your change touches SSH, remote parsing, or the file browser, also run the
control-plane suite against a real Linux target:

```bash
python tools/ssh_test_target.py start   # throwaway Alpine container with sshd
MINDASH_SSH_KEY=rust/.sshtarget/id_ed25519 ./rust/target/release/mindash &
python tools/audit_control.py
python tools/ssh_test_target.py stop
```

That suite exists because **shell quoting and remote parsing cannot be validated
on Windows** — `cmd.exe` has different rules, so a bug that matters over SSH
never surfaces locally. If you change quoting, it is the only test that counts.

## Tests are not optional here

Every bug in the [changelog](CHANGELOG.md) was found by a test rather than by
review, and several would have shipped in a state where the product did not work
at all. The one that stings most: the auth middleware matched public prefixes as
substrings, so `/login` swallowed `/api/auth/login` and nobody could sign in once
a password was set.

So: a behaviour change comes with a test that would fail without it. A bug fix
comes with a test that reproduces the bug first.

Write the test **before** fixing the bug where you can. It is the only way to
know the test actually exercises the failure.

## House rules

A few things are deliberate. Please don't undo them without saying why.

**Nothing shells out to build a command.** Wake-on-LAN writes its own packet.
Remote work goes through the system `ssh` client as an argv vector, and anything
that must reach the remote shell as one word is quoted by `exec::quote`, which has
tests for the characters that actually break. Remote scripts are base64-encoded so
the login shell cannot reinterpret them. A `sh -c` with interpolated input will
not be merged.

**Devices are addressed by id, never by host.** The host, user and port come from
stored config. If you add an endpoint, take an id.

**Actions are closed enums.** If you add an operation, add a variant. Do not add
a route that accepts a command string.

**Config writes stay atomic.** Serialise, write a temp file, rename. And a corrupt
file gets preserved, never overwritten.

**No polling loops.** Config changes travel over SSE. Widget data re-fetches on
the revision. If you need fresh data, hang it off the revision.

**Keep it one binary.** No database, no message broker, no runtime dependency.
That constraint is most of the point of the project.

## Code style

- `cargo fmt` and `cargo clippy -- -D warnings` are enforced in CI.
- Comments explain **why**, not what. If a line needs a comment to say what it
  does, rewrite the line. If it needs one to say why it is not the obvious thing,
  that comment is valuable — the codebase has a lot of those on purpose.
- Match the surrounding style. The code is written in British English in prose
  ("colour", "behaviour") and American English in identifiers (`ThemeColors`) to
  match the libraries.

## Commit messages

Conventional Commits, please — they feed the changelog:

```
feat(devices): show swap usage in the stats card
fix(files): quote the root before it reaches the remote shell
docs: correct the --data-dir default
```

Scope is optional but helpful: `core`, `server`, `web`, `tools`, `docs`.

## Reporting bugs

Open an issue with:

- what you did, what happened, and what you expected
- your OS and `mindash --version`
- the server log (`RUST_LOG=mindash=debug`)
- the relevant part of `config.json`, **with secrets and hostnames redacted**

## Security

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).

## Licence

By contributing you agree your work is licensed under the [MIT licence](LICENSE),
the same terms as the project.
