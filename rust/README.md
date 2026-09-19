# MinDash — implementation notes

`core/` is the shared model, `server/` is the axum HTTP server, and `web/` is the
Leptos dashboard compiled to WebAssembly.

## Layout

```
rust/
  core/     shared model: config, widget registry, wire types
  server/   axum HTTP server, auth, SSE, control plane, widget data providers
  web/      Leptos dashboard (client-side rendered, WASM)
  static/   served assets: shell, login page, icon, built bundle
```

The shared `core` crate is the main structural decision. The config shape, the
widget types and their validation are defined once and compiled into both sides,
so the server and the browser cannot disagree about what a widget is.

## Build

```bash
python tools/build_rust.py          # server binary + wasm bundle
python tools/build_rust.py --run    # ...then serve on :5050
cargo test --workspace              # unit tests
```

The wasm bundle is assembled with `cargo`, `wasm-opt` and `wasm-bindgen` directly
rather than through `trunk`. rustc emits bulk-memory operations that older
wasm-opt builds reject unless the feature is named, and trunk's `wasm_opt` config
key only selects a wasm-opt *version* — it does not forward flags. Running the
tools in sequence gives the same output without depending on that. `wasm-opt` is
optional: if it is missing the build proceeds with a larger module.

## Sizes

| | |
|---|---|
| Server binary | 4.3 MB (release, LTO, stripped) |
| Web bundle | ~1.1 MB (wasm + js + css) |
| Resident memory | ~7 MB |
| Startup | ~0.5 s |
| Cold build | ~2 min |

## Design notes

A few decisions worth knowing, because they are not the obvious ones.

**Devices are always addressed by id.** The host, user and port come from stored
config, never from the request. There is no endpoint that can be pointed at an
arbitrary machine. Actions are closed enums — a power action is one of three
variants, a container action is one of three — so a caller cannot express a
command.

**The control plane uses the system `ssh` client** rather than a pure-Rust SSH
implementation. Key-based auth, `~/.ssh/config`, agent forwarding, jump hosts and
`known_hosts` all keep working with no code, and there is no
`russh`/`ring`/`openssl` dependency tree. What is *not* carried over from the
usual way of doing this is string interpolation into a shell: every call is an
argv vector, anything that must reach the remote shell as one word is quoted by
`exec::quote`, and remote scripts are base64-encoded so they cannot be
reinterpreted by the login shell.

**The local machine is read natively, not over SSH.** Asking an sshd on the same
host for its own CPU would be slower and would require SSH to be configured on a
single-machine install for no reason. On Windows there is no POSIX shell, so
`win_stats` reads the same values natively and emits the identical shape for the
shared parser. CPU load and temperature are reported as absent on Windows rather
than making every other figure wait on the slow WMI providers that supply them —
that alone took a stats request from ~20 s to ~550 ms.

**Config writes are atomic.** Serialise, write to a sibling temp file, rename. A
crash mid-write cannot leave a truncated config, and a corrupt file is preserved
for inspection rather than overwritten. Concurrent writers serialise on a
dedicated lock held *after* the config lock is released, so a disk write never
blocks a reader.

**The dashboard does not poll.** One server-sent-events connection per client,
pushing only when the config revision changes. Widget data re-fetches on that
revision, so an idle dashboard makes no requests at all.

**Auth middleware returns the right thing per request kind.** An API call with no
session gets a `401` with a JSON body; a page request is redirected to `/login`.

## Status

| Suite | Result |
|---|---|
| `cargo test --workspace` | 95 unit tests |
| `tools/audit_rust.py` | 48 checks: config, widgets, hardening, branding |
| `tools/audit_auth.py` | 34 checks: password, sessions, cookie flags |
| `tools/audit_control.py` | 53 checks: control plane, over real SSH |
| `tools/verify_rust_ui.mjs` | 16 checks: dashboard, in a real browser |
| `tools/verify_control_ui.mjs` | 23 checks: devices, files, containers |

Implemented:

- config load/save with defaults merging, corrupt-file recovery and atomic writes
- auth: PBKDF2-HMAC-SHA256 with a self-describing hash format, HMAC-signed
  sessions, constant-time comparison, expiry in both directions
- widget registry, server-side validation, CRUD, and live data for all six types
- weather (wttr.in) and RSS/Atom providers, cached with stale-on-error
- SSE config stream, traversal hardening, SSRF-hardened widget data route
- remote stats: CPU, memory, temperature, uptime and disks in one round trip
- power: reboot, suspend, shutdown, with disconnect-aware success handling and a
  guard that refuses to power off the host itself
- wake-on-LAN, packet built in-process
- file browser confined to a configured root, with breadcrumb, listing and preview
- container start, stop and restart from a validated allowlist
- theme engine driven entirely by CSS custom properties, with accent ink chosen
  by measured contrast

Not implemented:

- the task scheduler
- forms for editing devices and file roots from the UI (the API and config support
  it; there is no UI yet)
- service worker and offline support
- mobile clients

### Testing the control plane

Shell quoting and remote parsing cannot be validated on Windows, whose shell has
different rules. `tools/ssh_test_target.py` starts a throwaway Alpine container
with `sshd` and a generated keypair:

```bash
python tools/ssh_test_target.py start
MINDASH_SSH_KEY=rust/.sshtarget/id_ed25519 ./rust/target/release/mindash &
python tools/audit_control.py
python tools/ssh_test_target.py stop   # removes the container and the key
```

`MINDASH_SSH_KEY` exists so the throwaway key never has to be written into config.
