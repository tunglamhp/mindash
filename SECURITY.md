# Security Policy

## Reporting a vulnerability

**Do not open a public issue.** Use GitHub's private reporting:

<https://github.com/tunglamhp/mindash/security/advisories/new>

Please include what you did, what happened, the impact as you see it, and your
`mindash --version`. A proof of concept helps enormously.

This is a small project maintained in spare time. You will get an
acknowledgement, and credit in the advisory and changelog unless you would rather
stay anonymous. Please give a reasonable window to fix it before disclosing
publicly.

## Please read this before deploying

**MinDash is remote administration software.** It runs with root privileges on
the machines it manages, reads and writes their filesystems, controls their
containers, and executes commands over SSH. Anyone who can reach the dashboard
can do all of that.

It is built for a trusted network. Treat it accordingly.

### Your responsibility

By installing and using MinDash you accept full responsibility for:

- securing your network and your systems
- restricting access, for example to a VPN such as WireGuard or Tailscale
- any damage resulting from misconfiguration or unauthorised access

This software is provided "as is", without warranty of any kind, under the
[MIT licence](LICENSE).

### Authentication is off by default

With no password set, **anyone who can reach the port has full control.** This is
deliberate — a first run on `localhost` should not require setup — and dangerous
the moment the port is reachable by anything else.

Set a password before you expose it to anything, even your LAN:

```bash
printf 'your-password' | mindash --set-password --data-dir /opt/mindash
```

The password is hashed with PBKDF2-HMAC-SHA256 at 210,000 iterations and stored
owner-only. The hash format carries its own parameters, so the iteration count can
be raised later without invalidating existing passwords.

### Never expose it directly to the internet

No TLS, no rate limiting, no lockout. Put it behind a VPN — WireGuard, Tailscale,
or similar — or a reverse proxy that terminates TLS and adds authentication.
Binding to `0.0.0.0` is the default; if you want loopback only, put it behind
something rather than relying on the bind address.

### What MinDash does to limit blast radius

These are properties worth knowing, and worth checking if you are auditing:

- **Devices are addressed by id.** The host, user and port come from stored
  config, never from a request. There is no endpoint that can be pointed at an
  arbitrary host.
- **Actions are closed enums.** A power action is one of three variants; a
  container action is one of three. A caller cannot express a command.
- **No shell interpolation.** Wake-on-LAN builds its own packet. Remote work uses
  the system `ssh` client with argv vectors; values reaching the remote shell as
  one word are single-quoted by a tested helper, and remote scripts are
  base64-encoded.
- **Widget data cannot become SSRF.** `/api/widget/{id}` takes an id; the URL
  fetched is derived from stored config, not from the request.
- **The file browser is confined.** Paths are normalised lexically and anything
  outside the configured root is refused.
- **Sessions are signed and scoped.** HMAC-SHA256, `HttpOnly`, `SameSite=Strict`,
  30-day expiry enforced in both directions (a clock jump cannot mint an immortal
  session), and the signature is verified before the payload is parsed.
- **Secrets are owner-only.** `.password` and `.session_secret` are written with
  `0600` where the platform supports it.

### What it does not do

- No TLS. Use a reverse proxy or a VPN.
- No rate limiting or account lockout on login.
- No multi-user accounts or roles. One password, full access.
- No audit log of who did what.
- No CSRF token. Mutating routes require a JSON content type and a
  `SameSite=Strict` session cookie, which blocks the ordinary cross-site form
  attack, but this is not a substitute for a proper token if you extend the API.

## Supported versions

Only the latest release gets security fixes.

| Version | Supported |
|---|---|
| 1.0.x | Yes |
| < 1.0 | No |
