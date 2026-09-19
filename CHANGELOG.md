# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Nothing yet.

## [1.0.0] — 2026-09-19

The first release: a complete homelab dashboard with a working control plane.

### Added

**Dashboard**

- Widget grid you configure from the UI — add, edit and remove without touching
  a file or restarting the server.
- Six widget types: weather (wttr.in), RSS/Atom news, clock, system overview,
  containers and links.
- Widget settings validated server-side against a typed registry: unknown keys
  dropped, numbers clamped to their declared range, selects falling back on an
  out-of-range value.
- Live data for every widget type, cached with stale-on-error so a flaky upstream
  degrades to the last good values instead of an error.
- 22 theme presets plus a live editor for every colour, radius and width.
- Contrast-safe ink on accent colours, chosen by measured contrast rather than
  hardcoded.

**Devices**

- Per-device cards with CPU load, memory, CPU temperature, uptime and
  per-filesystem usage, collected in a single SSH round trip.
- The machine MinDash runs on is read natively, with no SSH and no `sshd`
  required — including a native Windows collector, since Windows has no POSIX
  shell.

**Control**

- Reboot, suspend and shut down a device.
- Wake-on-LAN, with the magic packet assembled in-process rather than shelled out.
- Docker container start, stop and restart.

**Files**

- File browser confined to a per-device root, with breadcrumbs, a directory
  listing and a text preview.
- Paths normalised lexically, with anything outside the root refused.

**Server**

- Session authentication: PBKDF2-HMAC-SHA256 with a self-describing hash format,
  HMAC-signed session tokens, constant-time comparison and expiry enforced in
  both directions.
- `--set-password` reads from stdin so the password never appears in the shell
  history or in `ps` output, and the stored hash is written owner-only.
- Server-sent events for config changes: one connection per client, pushed only
  when the revision changes, so an idle dashboard makes no requests.
- Atomic config writes — serialise, write to a sibling temp file, rename — so a
  crash mid-write cannot leave a truncated config. A corrupt file is preserved
  for inspection and the server starts from defaults.
- `--help`, `--version`, `--print-config-dir`. A flag that needs a value and does
  not get one is an error rather than a silent fall back to the default.

### Security

- Devices are addressed by id only. The host, user and port come from stored
  config, never from a request, so no endpoint can be pointed at an arbitrary
  machine.
- Power and container actions are closed enums, so a caller cannot express a
  command.
- Remote work uses the system `ssh` client with argv vectors and no shell
  interpolation. Values reaching the remote shell as one word are quoted, and
  remote scripts are base64-encoded.
- Widget data is fetched only from URLs derived from stored config.
  `/api/widget/{id}` takes an id, never a URL, so it cannot be used as an SSRF
  primitive.
- **Authentication is off by default.** Set a password before exposing MinDash to
  any network, and keep it behind a VPN.

### Fixed during development

These are recorded because each was found by a test rather than by review, and
each would have shipped:

- The auth middleware matched public prefixes as raw substrings, so the `/login`
  prefix swallowed `/api/auth/login`. With a password set, the login endpoint
  itself answered 401 and there was no way to sign in at all.
- `stat -c` does not interpret backslash escapes. The file listing sent `\t` in
  its format string, `stat` printed it literally, and every directory listing
  came back empty.
- The SSH disconnect check matched `"timeout"`, which is not a substring of
  `"Connection timed out"`. An unreachable host was reported as a successful
  reboot.
- `df --output` pads columns with variable-width spaces, so field splitting put
  the data in the wrong columns.
- Windows drive roots were rejected by a Unix-only mount filter, so a Windows
  host showed no disks. A root of `A:/` also normalised to `A:`, which is the
  process's current directory on that drive rather than its root, so the root
  never matched its own listing.
- Concurrent config writes raced on a single shared temp path; one writer renamed
  it away and the next failed with "file not found".

### Known limitations

- No task scheduler.
- Devices and file roots can be configured through the API and the config file,
  but there is no UI for editing them.
- No service worker, so no offline support.
- No mobile clients.

[Unreleased]: https://github.com/tunglamhp/mindash/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/tunglamhp/mindash/releases/tag/v1.0.0
