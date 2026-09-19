#!/usr/bin/env python3
"""
Spin up a throwaway Linux host with sshd, for testing the SSH control plane.

Real remote command execution cannot be meaningfully tested on Windows: the local
shell is cmd.exe with different quoting rules, so a quoting bug that matters over
SSH would never surface. This starts a small Linux container with sshd and a
generated keypair, so `sysinfo`, power and Docker actions can be exercised the
way they will actually run.

Usage:
    python tools/ssh_test_target.py start     # create and start
    python tools/ssh_test_target.py stop      # stop and remove
    python tools/ssh_test_target.py status
    python tools/ssh_test_target.py shell "command"

Prints connection details on start. Nothing is left behind by `stop`.
"""
import argparse
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORK = ROOT / "rust" / ".sshtarget"
CONTAINER = "mindash-ssh-target"
SSH_PORT = 2222
IMAGE = "alpine:3.20"


def docker(*args, check=True):
    proc = subprocess.run(["docker", *args], capture_output=True, text=True)
    if check and proc.returncode != 0:
        raise SystemExit(f"docker {' '.join(args)} failed:\n{proc.stderr.strip()}")
    return proc.stdout.strip()


def have_image(ref):
    return subprocess.run(
        ["docker", "image", "inspect", ref],
        capture_output=True, text=True,
    ).returncode == 0


def running():
    out = docker("ps", "--filter", f"name=^{CONTAINER}$", "--format", "{{.ID}}", check=False)
    return bool(out.strip())


def harden_key(path: Path):
    """Restrict the private key to the current user.

    Windows OpenSSH refuses to use a key file that other principals can read, and
    a key generated inside the container lands with an inherited ACL. Without
    this, every connection fails with "UNPROTECTED PRIVATE KEY FILE".
    """
    if sys.platform != "win32":
        path.chmod(0o600)
        return
    user = subprocess.run(["whoami"], capture_output=True, text=True).stdout.strip()
    subprocess.run(["icacls", str(path), "/inheritance:r"], capture_output=True, text=True)
    subprocess.run(["icacls", str(path), "/grant:r", f"{user}:F"], capture_output=True, text=True)


def start():
    WORK.mkdir(parents=True, exist_ok=True)

    if not have_image(IMAGE):
        print(f"pulling {IMAGE} ...")
        docker("pull", IMAGE)

    # Keypair, generated once and reused so restarts keep the same host key pin.
    key = WORK / "id_ed25519"
    pub = WORK / "id_ed25519.pub"
    if not key.exists():
        print("generating an ssh keypair for the test target ...")
        docker("run", "--rm", "-v", f"{WORK.as_posix()}:/w", IMAGE, "sh", "-c",
               "apk add --no-cache openssh-keygen >/dev/null && "
               "ssh-keygen -t ed25519 -N '' -f /w/id_ed25519 -q")
    authorized = pub.read_text(encoding="utf-8").strip()
    harden_key(key)

    docker("rm", "-f", CONTAINER, check=False)

    # sshd in the foreground, with key auth only and no PAM.
    bootstrap = f"""
set -e
apk add --no-cache openssh >/dev/null 2>&1
ssh-keygen -A >/dev/null 2>&1
mkdir -p /root/.ssh
echo '{authorized}' > /root/.ssh/authorized_keys
chmod 700 /root/.ssh
chmod 600 /root/.ssh/authorized_keys
cat > /etc/ssh/sshd_config <<'EOF'
Port 22
PermitRootLogin prohibit-password
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
PidFile /run/sshd.pid
HostKey /etc/ssh/ssh_host_ed25519_key
EOF
exec /usr/sbin/sshd -D -e
"""
    docker("run", "-d", "--name", CONTAINER,
           "-p", f"127.0.0.1:{SSH_PORT}:22",
           IMAGE, "sh", "-c", bootstrap)

    print("waiting for sshd ...")
    for _ in range(60):
        out = docker("exec", CONTAINER, "sh", "-c", "test -f /run/sshd.pid && echo up", check=False)
        if "up" in out:
            break
        time.sleep(0.5)
    time.sleep(0.8)

    print(f"""
test target is up
  host      127.0.0.1
  port      {SSH_PORT}
  user      root
  key       {key}
  container {CONTAINER}

  docker exec {CONTAINER} sh -c "uname -a"
  ssh -i {key} -p {SSH_PORT} -o StrictHostKeyChecking=no root@127.0.0.1 'uname -a'
""")


def stop():
    docker("rm", "-f", CONTAINER, check=False)
    shutil.rmtree(WORK, ignore_errors=True)
    print("test target removed")


def status():
    if running():
        print(f"{CONTAINER} is running on 127.0.0.1:{SSH_PORT}")
        print(docker("exec", CONTAINER, "sh", "-c", "uname -a; echo; cat /etc/alpine-release", check=False))
    else:
        print(f"{CONTAINER} is not running")


def shell(cmd):
    if not running():
        raise SystemExit("test target is not running; run `start` first")
    key = WORK / "id_ed25519"
    proc = subprocess.run(
        ["ssh", "-i", str(key), "-p", str(SSH_PORT),
         "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
         "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
         "root@127.0.0.1", cmd],
        capture_output=True, text=True,
    )
    sys.stdout.write(proc.stdout)
    if proc.stderr.strip():
        sys.stderr.write(proc.stderr)
    return proc.returncode


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("action", choices=["start", "stop", "status", "shell"])
    ap.add_argument("command", nargs="*")
    args = ap.parse_args()

    if args.action == "start":
        start()
    elif args.action == "stop":
        stop()
    elif args.action == "status":
        status()
    else:
        return shell(" ".join(args.command))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
