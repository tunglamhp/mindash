# MinDash

**Your homelab, under control.**

A self-hosted dashboard for the machines you run at home: live system stats, power
control, a file browser and Docker container management, plus a grid of widgets —
weather, news, clock, system overview — that you add and configure from the UI
itself, with no plugins to install.

One Rust binary, one WebAssembly bundle, no runtime dependencies.

[![CI](https://github.com/tunglamhp/mindash/actions/workflows/ci.yml/badge.svg)](https://github.com/tunglamhp/mindash/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/tunglamhp/mindash?color=2ed573)](https://github.com/tunglamhp/mindash/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-2ed573.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.80%2B-2ed573.svg)](https://www.rust-lang.org)

---

## Why

Most homelab dashboards are a container, a database, a config file you edit by
hand, and a plugin ecosystem you have to learn. MinDash is one binary you point at
your machines. Widgets and devices are configured in the browser, validated
server-side, and stored as JSON you can read.

It is deliberately small: **4.3 MB** binary, **~7 MB** resident, **~0.5 s** to
start.

## Features

**Dashboard** — a widget grid you control from the UI. Add, configure and remove
widgets without touching a file or restarting anything.

| Widget | What it shows |
|---|---|
| Weather | Current conditions and a multi-day forecast, via wttr.in |
| News | RSS and Atom feeds, several at once |
| Clock | Local time with an optional date |
| Overview | Online/offline device counts, container total |
| Containers | Container states at a glance |
| Links | A list of bookmarks |

**Devices** — a card per machine with CPU load, memory, CPU temperature, uptime
and per-filesystem usage, collected in a **single SSH round trip** and parsed
from `/proc`. The machine MinDash runs on is read natively, with no SSH needed.

**Control** — reboot, suspend and shut down a host; wake it with a wake-on-LAN
magic packet; start, stop and restart Docker containers.

**Files** — a browser confined to a per-device root, with breadcrumbs, a
directory listing and a text preview.

**Themes** — 22 built-in presets plus a live editor for every colour, radius and
width. Themes are CSS custom properties written at runtime, so switching one
repaints without re-rendering a single component, and ink on accent colours is
chosen by **measured contrast** rather than hardcoded.

## Quick start

Requires **Rust 1.80+** and Python 3 (only to drive the build script).

```bash
git clone https://github.com/tunglamhp/mindash.git
cd mindash
python tools/build_rust.py --run
```

Then open <http://127.0.0.1:5050>.

The first start creates `rust/devdata/config.json` and generates the secrets it
needs. Nothing else is installed.

### Building the two artefacts separately

```bash
cargo build --release -p mindash-server --bin mindash   # the server
python tools/build_rust.py --web                        # wasm + js + css bundle
```

### Running it

```bash
./rust/target/release/mindash \
    --port 5050 \
    --data-dir /opt/mindash \
    --static-dir ./rust/static
```

| Flag | Default | Meaning |
|---|---|---|
| `--port` | `5050` | Listen port |
| `--data-dir` | `/opt/mindash` | Where `config.json` and secrets live |
| `--static-dir` | `static` | Where the web bundle and shell pages live |

`RUST_LOG=mindash=debug` raises the log level.

## Configuration

Everything is configured in the browser and stored as JSON. There is no YAML and
no config file to hand-edit — though the file is plain enough to edit if you
prefer.

A device looks like this:

```json
{
  "id": "nas",
  "name": "NAS",
  "ip": "192.168.1.20",
  "is_host": false,
  "ssh": { "user": "admin", "port": 22 },
  "files_root": "/srv",
  "wol": { "mac": "aa:bb:cc:dd:ee:ff", "broadcast": "192.168.1.255" },
  "docker": { "containers": ["plex", "sonarr"] }
}
```

Authentication is **off until you set a password**. Add one before exposing
MinDash to anything:

```bash
printf 'your-password' | ./rust/target/release/mindash --set-password --data-dir /opt/mindash
```

## How it fits together

```
rust/
  core/     shared model: config, widget registry, wire types
  server/   axum HTTP server, auth, SSE, control plane, widget providers
  web/      Leptos dashboard, client-side rendered to WebAssembly
  static/   shell pages, icon, built bundle
tools/      build and verification scripts
```

The shared `core` crate is the main structural decision: the config shape, the
widget types and their validation are defined once and compiled into both sides,
so the server and the browser cannot disagree about what a widget is.

## Design notes

A few decisions worth knowing, because they are not the obvious ones.

**Devices are always addressed by id.** The host, user and port come from stored
config, never from the request, so no endpoint can be pointed at an arbitrary
machine. Actions are closed enums — a power action is one of three variants, a
container action is one of three — so a caller cannot express a command.

**The control plane uses the system `ssh` client.** Key-based auth,
`~/.ssh/config`, agent forwarding, jump hosts and `known_hosts` all keep working
with no code, and there is no `russh`/`ring`/`openssl` dependency tree. What is
*not* carried over is string interpolation into a shell: every call is an argv
vector, anything that must reach the remote shell as one word is quoted by a
unit-tested helper, and remote scripts are base64-encoded so they cannot be
reinterpreted by the login shell.

**The local machine is read natively, not over SSH.** Asking an sshd on the same
host for its own CPU would be slower and would need SSH configured on a
single-machine install for no reason. On Windows there is no POSIX shell, so
`win_stats` reads the same values natively and emits the identical shape for the
shared parser.

**Config writes are atomic.** Serialise, write to a sibling temp file, rename. A
crash mid-write cannot leave a truncated config, and a corrupt file is preserved
for inspection rather than overwritten.

**The dashboard does not poll.** One server-sent-events connection per client,
pushing only when the config revision changes. Widget data re-fetches on that
revision, so an idle dashboard makes no requests at all.

## Development

```bash
cargo test --workspace                # 95 unit tests
cargo clippy --workspace --all-targets -- -D warnings
python tools/audit_rust.py            # 48 checks: config, widgets, hardening, branding
python tools/audit_auth.py            # 34 checks: password, sessions, cookie flags
node tools/verify_rust_ui.mjs         # dashboard, in a real browser
node tools/verify_control_ui.mjs      # devices, files, containers, in a browser
```

The control plane is tested against a real Linux host over SSH, because shell
quoting and remote parsing cannot be validated on Windows:

```bash
python tools/ssh_test_target.py start   # throwaway Alpine container with sshd
MINDASH_SSH_KEY=rust/.sshtarget/id_ed25519 ./rust/target/release/mindash &
python tools/audit_control.py           # 53 checks against the live host
python tools/ssh_test_target.py stop    # removes the container and the key
```

See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

## Status

**1.0.0 — usable, and honest about its edges.**

Implemented: widgets with server-side validation, SSH device stats, power
control, wake-on-LAN, Docker container actions, a confined file browser, the
theme engine, session auth, and an SSE config stream.

Not implemented yet:

- the task scheduler
- forms for editing devices and file roots from the UI — the API and config
  support them, but there is no UI
- a service worker for offline use
- mobile clients

## Security

MinDash runs with root privileges on the machines it manages and gives direct
access to your filesystem, your containers and a shell over SSH.

- **Authentication is off by default.** Set a password before you expose it to
  anything, even your LAN.
- **Never expose it directly to the internet.** Put it behind a VPN such as
  WireGuard or Tailscale.
- Widget data is fetched only from URLs derived from stored config. `/api/widget/{id}`
  takes an id, never a URL, so it cannot be turned into an SSRF primitive.
- The file browser normalises paths lexically and refuses anything outside the
  configured root.

See [SECURITY.md](SECURITY.md) for how to report a vulnerability.

## License

[MIT](LICENSE). Do what you like with it.
