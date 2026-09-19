#!/usr/bin/env python3
"""
Session auth audit.

Verifies that setting a password actually gates the API, and that signing in
works afterwards. Worth its own suite because the failure mode is total: if the
login route is itself caught by the auth middleware, every request is refused and
there is no way back in through the UI.

This suite starts and stops its own server on a free port. It has to: the password
is read once at start-up, so setting the file under a running server changes
nothing, and testing against a live instance would quietly assert the wrong thing.

Usage:
    python tools/audit_auth.py
"""
import argparse
import http.cookiejar
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PASSWORD = "correct-horse-battery-staple"
# Deliberately awkward: punctuation, a non-ASCII character, and **leading and
# trailing spaces**. Those edge spaces are the point: they catch an
# implementation that trims the submitted password before comparing, or that
# trims it on the way in. Interior spaces would not.
TRICKY = "  päss word with spaces  "

rows = []
G = ""


def group(name):
    global G
    G = name
    print(f"\n== {name} ==")


def rec(name, ok, detail=""):
    rows.append((G, name, ok))
    print(f"  {'ok  ' if ok else 'FAIL'} {name:<46} {detail}")


def binary() -> Path:
    name = "mindash.exe" if os.name == "nt" else "mindash"
    p = ROOT / "rust" / "target" / "release" / name
    if not p.exists():
        raise SystemExit(f"build first: {p} not found")
    return p


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def set_password(data_dir: Path, password: str, remove=False):
    """Drive the server's own --set-password so the test exercises real code."""
    if remove:
        for name in (".password",):
            p = data_dir / name
            if p.exists():
                p.unlink()
        return
    proc = subprocess.run(
        [str(binary()), "--set-password", "--data-dir", str(data_dir)],
        input=password.encode("utf-8"), capture_output=True,
    )
    if proc.returncode != 0:
        raise SystemExit(
            f"--set-password failed ({proc.returncode}): "
            f"{proc.stderr.decode('utf-8', 'replace').strip()}"
        )


class Server:
    """A MinDash instance this suite owns for the duration."""

    def __init__(self, data_dir: Path):
        self.data_dir = data_dir
        self.port = free_port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.proc = None
        self.log = data_dir / "server.log"

    def start(self):
        self.proc = subprocess.Popen(
            [str(binary()),
             "--port", str(self.port),
             "--data-dir", str(self.data_dir),
             "--static-dir", str(ROOT / "rust" / "static")],
            stdout=self.log.open("w"), stderr=subprocess.STDOUT,
        )
        deadline = time.time() + 20
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise SystemExit(
                    f"server exited with {self.proc.returncode}:\n"
                    f"{self.log.read_text(encoding='utf-8', errors='replace')}"
                )
            try:
                with urllib.request.urlopen(f"{self.base}/login", timeout=1):
                    return
            except urllib.error.HTTPError:
                return
            except Exception:
                time.sleep(0.2)
        raise SystemExit("server did not come up")

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()


class Client:
    """A browser-like client with its own cookie jar."""

    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.jar)
        )
        # Python's CookieJar drops the HttpOnly flag, so the header is captured
        # raw and asserted on directly.
        self.last_set_cookie = ""

    def go(self, base, method, path, payload=None):
        data = json.dumps(payload).encode() if payload is not None else None
        req = urllib.request.Request(
            base + path, method=method, data=data,
            headers={"Content-Type": "application/json"},
        )
        try:
            with self.opener.open(req, timeout=20) as r:
                self.last_set_cookie = r.headers.get("Set-Cookie", "")
                raw = r.read()
                try:
                    return r.status, json.loads(raw or b"{}")
                except (json.JSONDecodeError, UnicodeDecodeError):
                    # Not every route is JSON: `/` and `/login` serve HTML.
                    return r.status, {"_html_bytes": len(raw)}
        except urllib.error.HTTPError as e:
            self.last_set_cookie = e.headers.get("Set-Cookie", "") if e.headers else ""
            try:
                return e.code, json.loads(e.read() or b"{}")
            except Exception:
                return e.code, {}

    def cookie(self, name):
        return next((c for c in self.jar if c.name == name), None)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true",
                    help="leave the temporary data directory in place")
    args = ap.parse_args()

    data_dir = Path(tempfile.mkdtemp(prefix="mindash-auth-"))
    server = Server(data_dir)

    group("CLI")
    proc = subprocess.run([str(binary()), "--version"], capture_output=True, text=True)
    rec("--version works", proc.returncode == 0 and "mindash" in proc.stdout,
        proc.stdout.strip())
    proc = subprocess.run([str(binary()), "--help"], capture_output=True, text=True)
    rec("--help works", proc.returncode == 0 and "USAGE" in proc.stdout)
    for label, argv in [
        ("a flag with no value", ["--port"]),
        ("a bad port value", ["--port", "nope"]),
        ("an unknown flag", ["--bogus"]),
    ]:
        proc = subprocess.run([str(binary()), *argv], capture_output=True, text=True)
        rec(f"{label} is rejected", proc.returncode == 2, f"exit {proc.returncode}")
    proc = subprocess.run(
        [str(binary()), "--set-password", "--data-dir", str(data_dir)],
        input=b"short", capture_output=True,
    )
    rec("a short password is refused", proc.returncode == 2, f"exit {proc.returncode}")

    try:
        group("Auth disabled (the default)")
        server.start()
        anon = Client()
        st, body = anon.go(server.base, "GET", "/api/config")
        rec("the API is open with no password", st == 200, f"HTTP {st}")
        rec("auth_enabled is reported false", body.get("auth_enabled") is False,
            str(body.get("auth_enabled")))

        group("With a password set")
        server.stop()
        set_password(data_dir, PASSWORD)
        server.start()

        anon = Client()
        st, _ = anon.go(server.base, "GET", "/api/config")
        rec("api/config is refused anonymously", st == 401, f"HTTP {st}")
        st, _ = anon.go(server.base, "GET", "/api/health")
        rec("api/health is refused anonymously", st == 401, f"HTTP {st}")
        st, _ = anon.go(server.base, "GET", "/api/devices/status")
        rec("control routes are refused anonymously", st == 401, f"HTTP {st}")
        st, body = anon.go(server.base, "GET", "/")
        rec("a page request serves the login page", st == 200 and "_html_bytes" in body,
            f"HTTP {st}")
        st, _ = anon.go(server.base, "GET", "/icon.svg")
        rec("the icon stays public", st == 200, f"HTTP {st}")
        st, _ = anon.go(server.base, "GET", "/manifest.webmanifest")
        rec("the manifest stays public", st == 200, f"HTTP {st}")

        group("Signing in")
        st, _ = anon.go(server.base, "POST", "/api/auth/login", {"password": "wrong"})
        rec("a wrong password is refused", st == 401, f"HTTP {st}")
        st, _ = anon.go(server.base, "POST", "/api/auth/login", {})
        rec("a missing password is refused", st in (400, 401, 422), f"HTTP {st}")

        session = Client()
        st, body = session.go(server.base, "POST", "/api/auth/login", {"password": PASSWORD})
        # Captured immediately: a later request overwrites the stored header.
        login_cookie_header = session.last_set_cookie
        rec("the correct password is accepted", st == 200 and body.get("success") is True,
            f"HTTP {st}")
        st, body = session.go(server.base, "GET", "/api/config")
        rec("the session unlocks the API", st == 200 and body.get("success") is True,
            f"HTTP {st}")
        rec("auth_enabled is reported true", body.get("auth_enabled") is True,
            str(body.get("auth_enabled")))
        st, _ = session.go(server.base, "GET", "/api/devices/status")
        rec("the session unlocks control routes", st == 200, f"HTTP {st}")

        group("Session hygiene")
        other = Client()
        st, _ = other.go(server.base, "GET", "/api/config")
        rec("a session does not leak to another client", st == 401, f"HTTP {st}")

        cookie = session.cookie("mindash_session")
        rec("the session cookie exists", cookie is not None,
            cookie.name if cookie else "none")
        header = login_cookie_header
        rec("it is HttpOnly", "httponly" in header.lower(), header[:56])
        rec("it is SameSite=Strict", "samesite=strict" in header.lower())
        rec("it is scoped to the whole site", "path=/" in header.lower())

        forged = Client()
        forged.jar.set_cookie(http.cookiejar.Cookie(
            version=0, name="mindash_session", value="abc.def", port=None,
            port_specified=False, domain="127.0.0.1", domain_specified=False,
            domain_initial_dot=False, path="/", path_specified=True, secure=False,
            expires=None, discard=True, comment=None, comment_url=None, rest={},
            rfc2109=False,
        ))
        st, _ = forged.go(server.base, "GET", "/api/config")
        rec("a forged session cookie is refused", st == 401, f"HTTP {st}")

        st, _ = session.go(server.base, "POST", "/api/auth/logout")
        rec("logout succeeds", st == 200, f"HTTP {st}")
        st, _ = session.go(server.base, "GET", "/api/config")
        rec("the session is dead after logout", st == 401, f"HTTP {st}")

        group("Unusual passwords")
        server.stop()
        set_password(data_dir, TRICKY)
        server.start()
        tricky = Client()
        st, _ = tricky.go(server.base, "POST", "/api/auth/login", {"password": TRICKY})
        rec("edge whitespace and non-ASCII survive the round trip", st == 200, f"HTTP {st}")
        st, _ = tricky.go(server.base, "POST", "/api/auth/login",
                          {"password": TRICKY.strip()})
        rec("a whitespace-trimmed variant is refused", st == 401, f"HTTP {st}")
        st, _ = tricky.go(server.base, "POST", "/api/auth/login",
                          {"password": TRICKY + " "})
        rec("an extra trailing space is refused", st == 401, f"HTTP {st}")

        group("The stored credential")
        pw_file = data_dir / ".password"
        stored = pw_file.read_text(encoding="utf-8")
        rec("the password is hashed, not stored in clear", PASSWORD not in stored
            and TRICKY not in stored)
        rec("the hash is self-describing", stored.startswith("pbkdf2$sha256$"),
            stored[:24] + "...")
        rec("the iteration count is recorded", "$210000$" in stored)
        if os.name != "nt":
            mode = oct(pw_file.stat().st_mode & 0o777)
            rec("the hash file is owner-only", mode == "0o600", mode)
    finally:
        server.stop()
        if args.keep:
            print(f"\ndata directory kept at {data_dir}")
        else:
            shutil.rmtree(data_dir, ignore_errors=True)

    failed = [r for r in rows if not r[2]]
    print("\n" + "=" * 66)
    print(f"  {len(rows) - len(failed)}/{len(rows)} auth checks passed")
    if failed:
        print("  failures:")
        for g, n, _ in failed:
            print(f"    [{g}] {n}")
    print("=" * 66)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
