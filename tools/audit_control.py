#!/usr/bin/env python3
"""
Control-plane integration test.

Runs against a real Linux target over SSH (start one with
`python tools/ssh_test_target.py start`) and exercises the endpoints the way the
dashboard does. This is the test that matters: command quoting and remote
parsing cannot be validated against a Windows shell, which has different rules.

Usage:
    python tools/ssh_test_target.py start
    MINDASH_SSH_KEY=rust/.sshtarget/id_ed25519 python tools/audit_control.py
"""
import http.cookiejar
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

PORT = int(os.environ.get("MINDASH_PORT", "5050"))
BASE = f"http://127.0.0.1:{PORT}"
DEVICE = "testhost"
ROOT = "/etc"

jar = http.cookiejar.CookieJar()
opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))

rows = []
G = ""


def group(name):
    global G
    G = name
    print(f"\n== {name} ==")


def rec(name, ok, detail=""):
    rows.append((G, name, ok))
    print(f"  {'ok  ' if ok else 'FAIL'} {name:<48} {detail}")


def call(method, path, payload=None, timeout=60):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        BASE + path, method=method, data=data,
        headers={"Content-Type": "application/json"},
    )
    try:
        with opener.open(req, timeout=timeout) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read() or b"{}")
        except Exception:
            return e.code, {}
    except Exception as e:
        return 0, {"error": str(e)}


def configure():
    """Point a device at the SSH test target, confined to ROOT."""
    st, res = call("GET", "/api/config")
    cfg = res.get("config", {})
    host = next((d for d in cfg.get("devices", []) if d.get("is_host")), None)
    devices = [host] if host else []
    devices.append({
        "id": DEVICE,
        "name": "Test Host",
        "ip": "127.0.0.1",
        "icon": "server",
        "is_host": False,
        "ssh": {"user": "root", "port": 2222},
        "files_root": ROOT,
        "docker": {"containers": []},
    })
    cfg["devices"] = devices
    st, res = call("POST", "/api/config", cfg)
    return res.get("success") is True


group("Setup")
if not configure():
    print("  could not write config; is the server running?")
    sys.exit(2)
print(f"  configured device {DEVICE} (files_root={ROOT})")

group("Device status")
st, res = call("GET", "/api/devices/status")
devs = {d["id"]: d for d in res.get("data", {}).get("devices", [])}
rec("status lists every device", st == 200 and DEVICE in devs, f"{len(devs)} devices")
rec("test host reports online", devs.get(DEVICE, {}).get("online") is True,
    str(devs.get(DEVICE, {}).get("online")))

group("Remote stats (real SSH)")
st, res = call("GET", f"/api/devices/{DEVICE}/stats")
d = res.get("data", {})
rec("stats request succeeds", st == 200 and res.get("success") is True,
    str(res.get("error", ""))[:60])
if st == 200:
    rec("cpu is a percentage",
        isinstance(d.get("cpu"), int) and 0 <= d["cpu"] <= 100, f"cpu={d.get('cpu')}")
    rec("ram total is sane", (d.get("ram_total") or 0) > 100_000_000,
        f"{round((d.get('ram_total') or 0) / 1024**3, 1)} GiB")
    rec("ram used <= total", (d.get("ram_used") or 0) <= (d.get("ram_total") or 0))
    rec("uptime formatted", isinstance(d.get("uptime"), str) and d["uptime"].endswith("h"),
        str(d.get("uptime")))
    disks = d.get("disks") or []
    rec("root filesystem found", any(x["mount"] == "/" for x in disks), f"{len(disks)} disks")
    if disks:
        root = next((x for x in disks if x["mount"] == "/"), disks[0])
        rec("disk total is bytes", root["total"] > 1_000_000_000, str(root["total"]))
        rec("disk used <= total", root["used"] <= root["total"])

group("Unknown device")
st, res = call("GET", "/api/devices/nope/stats")
rec("unknown id -> 404", st == 404, f"HTTP {st}")
st, res = call("POST", "/api/devices/nope/power", {"action": "reboot"})
rec("unknown id power -> 404", st == 404, f"HTTP {st}")

group("Power action validation")
st, res = call("POST", f"/api/devices/{DEVICE}/power", {"action": "rm -rf /"})
rec("arbitrary action rejected", st >= 400, f"HTTP {st}")
st, res = call("POST", f"/api/devices/{DEVICE}/power", {"action": "hibernate"})
rec("unknown action rejected", st >= 400, f"HTTP {st}")
st, res = call("POST", "/api/devices/host/power", {"action": "shutdown"})
rec("host shutdown refused", st == 400 and "Refusing" in str(res.get("error", "")),
    str(res.get("error", ""))[:48])

group("Wake-on-LAN")
st, res = call("POST", f"/api/devices/{DEVICE}/wake")
rec("no MAC configured -> 400", st == 400, str(res.get("error", ""))[:48])


def set_wol(mac, broadcast="127.0.0.1"):
    st, cfgres = call("GET", "/api/config")
    cfg = cfgres["config"]
    for dev in cfg["devices"]:
        if dev["id"] == DEVICE:
            dev["wol"] = {"mac": mac, "broadcast": broadcast}
    call("POST", "/api/config", cfg)


set_wol("aa:bb:cc:dd:ee:ff")
st, res = call("POST", f"/api/devices/{DEVICE}/wake")
rec("wake succeeds with a MAC", st == 200 and res.get("success") is True,
    str(res.get("broadcast", "")))
set_wol("zz:zz:zz:zz:zz:zz")
st, res = call("POST", f"/api/devices/{DEVICE}/wake")
rec("invalid MAC rejected", st == 400, str(res.get("error", ""))[:48])

group(f"Files (root = {ROOT})")
st, res = call("GET", f"/api/devices/{DEVICE}/files?path=")
rec("listing succeeds", st == 200 and res.get("success") is True, str(res.get("error", ""))[:60])
if st == 200:
    entries = res.get("entries", [])
    names = [e["name"] for e in entries]
    rec("listing has entries", len(entries) > 0, f"{len(entries)} entries")
    dirs = [e for e in entries if e["is_dir"]]
    rec("directories sort first", entries[:len(dirs)] == dirs, f"{len(dirs)} dirs")
    rec("names are final components", all("/" not in n for n in names), str(names[:2]))
    rec("paths are absolute", all(e["path"].startswith("/") for e in entries),
        entries[0]["path"] if entries else "")
    rec("path resolves to the root", res.get("path") == ROOT, str(res.get("path")))
    rec("mode bits present", all(len(e["mode"]) == 10 for e in entries), entries[0]["mode"])
    rec("hostname file is listed", "hostname" in names, str(sorted(names)[:4]))
    rec("root has no parent", res.get("parent") is None, str(res.get("parent")))
    rec("at_root is true", res.get("at_root") is True)

st, res = call("GET", f"/api/devices/{DEVICE}/files?path=" + urllib.parse.quote("/etc/apk"))
rec("descend into a subdirectory", st == 200 and res.get("path") == "/etc/apk",
    str(res.get("path")))
rec("subdirectory has a parent", res.get("parent") == "/etc", str(res.get("parent")))

st, res = call("GET", f"/api/devices/{DEVICE}/file?path=" + urllib.parse.quote("/etc/hostname"))
rec("read a file", st == 200 and res.get("success") is True, repr(str(res.get("content", ""))[:32]))
st, res = call("GET", f"/api/devices/{DEVICE}/file?path=" + urllib.parse.quote("/etc/"))
rec("reading a directory is refused", st >= 400, f"HTTP {st}")

group("Path confinement")
# These genuinely resolve outside the configured root.
for bad in ["/root", "/", "/home", "/etc/../root/.ssh/id_rsa", "/etc/../../root",
            "/var", "/etc/../var/log", "/root/.ssh"]:
    st, res = call("GET", f"/api/devices/{DEVICE}/files?path=" + urllib.parse.quote(bad))
    rec(f"listing blocked {bad[:28]}", st == 403, f"HTTP {st}")
for bad in ["/root/.ssh/id_rsa", "/", "/etc/../root/.ssh/id_rsa"]:
    st, res = call("GET", f"/api/devices/{DEVICE}/file?path=" + urllib.parse.quote(bad))
    rec(f"read blocked {bad[:26]}", st == 403, f"HTTP {st}")

# A path using `..` that stays inside the root is *normalised*, not rejected.
# `/etc/../../etc/x` collapses to `/etc/x`, which is already permitted, so
# refusing it would be theatre rather than security. What must never happen is
# landing outside the root, which the cases above cover.
for path, want in [("/etc/apk/../apk", "/etc/apk"),
                   ("/etc/../../etc", "/etc"),
                   ("/etc/./apk", "/etc/apk")]:
    st, res = call("GET", f"/api/devices/{DEVICE}/files?path=" + urllib.parse.quote(path))
    inside = str(res.get("path", "")).startswith(ROOT)
    rec(f"{path[:24]} stays inside the root", st == 200 and res.get("path") == want and inside,
        str(res.get("path")))

group("Containers")
st, res = call("GET", "/api/containers")
rec("container listing", st == 200 and res.get("success") is True,
    f"{len(res.get('containers', []))} shown")
st, res = call("GET", "/api/containers/available")
rec("available listing", st == 200 and isinstance(res.get("names"), list),
    f"{len(res.get('names', []))} names on the host")
st, res = call("POST", "/api/containers/action", {"name": "not-configured", "action": "restart"})
rec("unconfigured container refused", st == 403, str(res.get("error", ""))[:44])
st, res = call("POST", "/api/containers/action", {"name": "x; rm -rf /", "action": "restart"})
rec("hostile name refused", st >= 400, f"HTTP {st}")
st, res = call("POST", "/api/containers/action", {"name": "x", "action": "exec"})
rec("arbitrary action refused", st >= 400, f"HTTP {st}")

group("SSRF surface")
st, res = call("GET", "/api/devices/127.0.0.1/stats")
rec("ip-shaped device id -> 404", st == 404, f"HTTP {st}")
st, res = call("GET", f"/api/devices/{DEVICE}/files?path=" + urllib.parse.quote("//root"))
rec("doubled-leading-slash does not escape", st == 403, f"HTTP {st}")

failed = [r for r in rows if not r[2]]
print("\n" + "=" * 66)
print(f"  {len(rows) - len(failed)}/{len(rows)} control-plane checks passed")
if failed:
    print("  failures:")
    for g, n, _ in failed:
        print(f"    [{g}] {n}")
print("=" * 66)
sys.exit(1 if failed else 0)
