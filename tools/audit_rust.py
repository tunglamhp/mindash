#!/usr/bin/env python3
"""
Feature audit for the Rust/Leptos implementation.

Mirrors tools/audit_features.py but targets the Rust server, so the two stacks
can be compared on the same criteria.

Usage: python tools/audit_rust.py [--port 8138]
"""
import http.cookiejar
import json
import sys
import urllib.error
import urllib.request

PORT = 8138
for i, a in enumerate(sys.argv):
    if a == "--port" and i + 1 < len(sys.argv):
        PORT = int(sys.argv[i + 1])
BASE = f"http://127.0.0.1:{PORT}"

jar = http.cookiejar.CookieJar()
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))

rows = []
def rec(group, name, ok, detail=""):
    rows.append((group, name, ok))
    print(f"  {'ok  ' if ok else 'FAIL'} {name:<46} {detail}")

def call(method, path, payload=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        BASE + path, method=method, data=data,
        headers={"Content-Type": "application/json"},
    )
    try:
        with opener.open(req, timeout=30) as r:
            raw = r.read()
            try:
                return r.status, json.loads(raw or b"{}")
            except (json.JSONDecodeError, UnicodeDecodeError):
                return r.status, {"_bytes": len(raw)}
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw or b"{}")
        except (json.JSONDecodeError, UnicodeDecodeError):
            return e.code, {"_bytes": len(raw)}
    except Exception as e:
        return 0, {"error": str(e)}


def call_raw(method, path, timeout=30):
    """Fetch a path and return its raw bytes, for substring checks."""
    req = urllib.request.Request(BASE + path, method=method)
    try:
        with opener.open(req, timeout=timeout) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()
    except Exception:
        return 0, b""


G = ""
def group(n):
    global G
    G = n
    print(f"\n== {n} ==")


group("Stack")
st, res = call("GET", "/api/version")
rec(G, "version identifies runtime", st == 200 and res.get("runtime") == "rust", res.get("runtime", ""))

group("Static shell & branding")
for path in ["/", "/login", "/icon.svg", "/manifest.webmanifest"]:
    st, res = call("GET", path)
    rec(G, f"serves {path}", st == 200, f"{res.get('_bytes', 0)} bytes")

# The icon and manifest are fetched while the login page parses, so they must be
# reachable without a session or the tab shows a placeholder.
st, _ = call("GET", "/icon.svg")
rec(G, "icon needs no session", st == 200, f"HTTP {st}")

st, res = call("GET", "/api/version")
rec(G, "reports MinDash", res.get("name") == "MinDash", str(res.get("name")))
rec(G, "reports version 1.0.0", res.get("version") == "1.0.0", str(res.get("version")))

# The font and wallpaper bundles were removed, along with their routes. These
# must now 404 rather than serve a stale file from a directory that is gone.
for path in ["/fonts/MinDash-Font-Regular.ttf", "/wallpapers/mindash-wallpaper-phone-oled.jpg"]:
    st, _ = call("GET", path)
    rec(G, f"removed route {path.split('/')[1]}", st == 404, f"HTTP {st}")

for path in ["/", "/login", "/assets/mindash-web.js", "/icon.svg",
             "/manifest.webmanifest", "/assets/mindash-web.css"]:
    st, raw = call_raw("GET", path)
    body = raw.decode("utf-8", "replace")
    leaks = [w for w in ("deq", "DeQ", "DEQ", "CC BY-NC", "NonCommercial", "Creative Commons")
             if w in body]
    rec(G, f"no upstream name or old licence in {path}", not leaks, ", ".join(leaks))

group("Traversal & input hardening")
for bad in ["/assets/../Cargo.toml", "/icon.svg/../../Cargo.toml",
            "/api/widget/../../etc/passwd", "/%2e%2e/Cargo.toml"]:
    st, res = call("GET", bad)
    rec(G, f"blocks {bad[:38]}", st in (400, 404), f"HTTP {st}")
group("Config")
st, res = call("GET", "/api/config")
cfg = res.get("config", {})
rec(G, "config document", st == 200 and isinstance(cfg, dict), f"{len(res.get('_bytes', []))} bytes")
rec(G, "host device present", any(d.get("is_host") for d in cfg.get("devices", [])))
rec(G, "settings carry theme defaults",
    cfg.get("settings", {}).get("theme", {}).get("accent") == "#2ed573",
    cfg.get("settings", {}).get("theme", {}).get("accent", ""))
rec(G, "widget types shipped with config", len(res.get("widget_types", [])) == 6,
    f"{len(res.get('widget_types', []))} types")
st, res = call("GET", "/api/widget-types")
rec(G, "widget-types endpoint", st == 200 and len(res.get("types", [])) == 6)
for t in res.get("types", []):
    if not t.get("fields"):
        rec(G, f"type {t.get('kind')} has fields", False)
rec(G, "every type exposes fields",
    all(t.get("fields") for t in res.get("types", [])))

group("Widgets")
made = {}
for kind in ["weather", "clock", "rss", "overview", "containers", "links"]:
    st, res = call("POST", "/api/widgets", {"type": kind})
    if res.get("success"):
        made[kind] = res["widget"]["id"]
rec(G, "create one of every type", len(made) == 6, f"{len(made)}/6")
rec(G, "new widget carries coerced defaults",
    call("GET", "/api/config")[1]["config"]["widgets"][0]["settings"] != {})

st, res = call("POST", "/api/widgets", {"type": "not-a-type"})
rec(G, "unknown widget type rejected", st >= 400, f"HTTP {st}")

wid = made.get("weather")
if wid:
    st, res = call("POST", f"/api/widgets/{wid}",
                   {"settings": {"location": "Tokyo", "days": 99, "evil": "x"}})
    s = res.get("widget", {}).get("settings", {})
    rec(G, "settings persist", s.get("location") == "Tokyo", str(s.get("location")))
    rec(G, "numbers clamped", s.get("days") == 5, f"days={s.get('days')}")
    rec(G, "unknown keys dropped", "evil" not in s, str(sorted(s)))
    st, res = call("POST", f"/api/widgets/{wid}", {"settings": {"units": "kelvin"}})
    rec(G, "bad select falls back", res["widget"]["settings"]["units"] == "c",
        res["widget"]["settings"]["units"])
    st, res = call("POST", f"/api/widgets/{wid}", {"settings": {"days": "2"}})
    rec(G, "numeric string accepted", res["widget"]["settings"]["days"] == 2,
        str(res["widget"]["settings"]["days"]))

group("Widget data")
for kind, w in made.items():
    st, res = call("GET", f"/api/widget/{w}")
    d = res.get("data") or {}
    if "error" in d:
        rec(G, f"data: {kind}", True, f"upstream unavailable ({str(d['error'])[:34]})")
    else:
        rec(G, f"data: {kind}", st == 200 and res.get("success"), "live")

group("SSRF surface")
st, res = call("GET", "/api/widget/does-not-exist")
rec(G, "unknown widget id -> 404", st == 404, f"HTTP {st}")
st, res = call("GET", "/api/widget/http%3A%2F%2F169.254.169.254%2F")
rec(G, "url-shaped id is never fetched", st == 404 and "data" not in res, f"HTTP {st}")

group("Config-save guard")
st, cfgres = call("GET", "/api/config")
full = cfgres["config"]
before = len(full.get("widgets", []))
# A client that saves the whole config without the widgets key must not wipe the
# widget list -- older clients did exactly that.
without_widgets = {k: v for k, v in full.items() if k != "widgets"}
st, res = call("POST", "/api/config", without_widgets)
rec(G, "save without widgets accepted", res.get("success") is True, str(res.get("error", "")))
st, cfgres = call("GET", "/api/config")
after = len(cfgres["config"].get("widgets", []))
rec(G, "widgets survive a save without them", after == before, f"{before} -> {after}")

group("Widget delete")
victim = made.get("clock")
st, res = call("DELETE", f"/api/widgets/{victim}")
rec(G, "delete succeeds", st in (200, 204), f"HTTP {st}")
st, res = call("DELETE", f"/api/widgets/{victim}")
rec(G, "double delete -> 404", st == 404, f"HTTP {st}")
st, cfgres = call("GET", "/api/config")
rec(G, "widget gone from config",
    victim not in [w["id"] for w in cfgres["config"].get("widgets", [])])

group("SSE")
try:
    req = urllib.request.Request(BASE + "/api/stream")
    with opener.open(req, timeout=6) as r:
        ctype = r.headers.get("Content-Type", "")
        rec(G, "stream opens", "text/event-stream" in ctype, ctype)
except Exception as e:
    rec(G, "stream opens", False, str(e)[:40])

failed = [r for r in rows if not r[2]]
print("\n" + "=" * 64)
print(f"  {len(rows) - len(failed)}/{len(rows)} checks passed")
if failed:
    print("  failures:")
    for g, n, _ in failed:
        print(f"    [{g}] {n}")
print("=" * 64)
sys.exit(1 if failed else 0)
