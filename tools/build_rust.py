#!/usr/bin/env python3
"""
Build the Rust/Leptos stack.

Two artifacts:

  server  native binary          -> rust/target/release/mindash[.exe]
  web     wasm + js + css bundle -> rust/static/assets/

wasm-bindgen is invoked directly rather than through `trunk build`, because
rustc 1.94 emits bulk-memory operations that the wasm-opt bundled with trunk
0.21 (version 123) rejects, and trunk's `wasm_opt` config key only selects a
wasm-opt *version* -- it does not forward the `--enable-bulk-memory-opt` flag the
validator needs. Driving cargo and wasm-bindgen by hand sidesteps the tool
entirely and produces the same bundle.

Usage:
    python tools/build_rust.py              # build both
    python tools/build_rust.py --server     # native only
    python tools/build_rust.py --web        # wasm only
    python tools/build_rust.py --run        # build, then serve on :5050
"""
import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUST = ROOT / "rust"
WEB = RUST / "web"
ASSETS = RUST / "static" / "assets"
WASM_CRATE = RUST / "target" / "wasm32-unknown-unknown" / "release" / "mindash-web.wasm"


def run(cmd, cwd, label):
    print(f"\n== {label} ==")
    print("   " + " ".join(str(c) for c in cmd))
    env = dict(os.environ, NO_COLOR="true")
    proc = subprocess.run([str(c) for c in cmd], cwd=str(cwd), env=env)
    if proc.returncode != 0:
        raise SystemExit(f"{label} failed with exit code {proc.returncode}")


def find_wasm_bindgen():
    """Prefer one on PATH, else the copy trunk cached."""
    found = shutil.which("wasm-bindgen")
    if found:
        return Path(found)
    cache = Path(os.environ.get("LOCALAPPDATA", "")) / "trunkrs" / "trunk" / "cache"
    if cache.exists():
        candidates = sorted(cache.glob("wasm-bindgen-*/wasm-bindgen.exe"), reverse=True)
        if candidates:
            return candidates[0]
    raise SystemExit(
        "wasm-bindgen not found. Install it with:\n"
        "    cargo install wasm-bindgen-cli\n"
        "or run `trunk build` once, which downloads a copy."
    )


def build_server(release=True):
    cmd = ["cargo", "build", "-p", "mindash-server", "--bin", "mindash"]
    if release:
        cmd.append("--release")
    run(cmd, RUST, "building the server")
    name = "mindash.exe" if os.name == "nt" else "mindash"
    profile = "release" if release else "debug"
    binary = RUST / "target" / profile / name
    if binary.exists():
        print(f"\n   server: {binary}  ({binary.stat().st_size / 1024 / 1024:.2f} MB)")
    return binary


def build_web():
    run(["cargo", "build", "--release", "--target", "wasm32-unknown-unknown", "-p", "mindash-web"],
        RUST, "building the wasm crate")
    if not WASM_CRATE.exists():
        raise SystemExit(f"expected {WASM_CRATE} to exist after the build")

    ASSETS.mkdir(parents=True, exist_ok=True)

    # wasm-opt before wasm-bindgen: it shrinks the module, and bindgen then
    # generates JS matching the optimised names.
    source = optimise(WASM_CRATE)

    bindgen = find_wasm_bindgen()
    run([bindgen, "--target", "web", "--no-typescript",
         "--out-dir", ASSETS, "--out-name", "mindash-web", source],
        RUST, "running wasm-bindgen")

    shutil.copyfile(WEB / "style.css", ASSETS / "mindash-web.css")

    total = 0
    print("\n   bundle:")
    for f in sorted(ASSETS.iterdir()):
        if f.is_file():
            total += f.stat().st_size
            print(f"     {f.name:<22} {f.stat().st_size / 1024:>8.1f} KB")
    print(f"     {'total':<22} {total / 1024:>8.1f} KB")
    return ASSETS


def optimise(wasm: Path) -> Path:
    """Run wasm-opt over the module, or return it unchanged.

    wasm-opt is optional: the build is correct without it, just larger. The
    feature flags are required because rustc emits bulk-memory operations that an
    older validator rejects by default.
    """
    opt = find_wasm_opt()
    if opt is None:
        print("\n   wasm-opt not found; shipping an unoptimised module")
        return wasm

    out = wasm.with_name(wasm.stem + "-opt.wasm")
    cmd = [opt, "-Oz",
           "--enable-bulk-memory", "--enable-bulk-memory-opt",
           "--enable-nontrapping-float-to-int", "--enable-sign-ext",
           "-o", out, wasm]
    print("\n== optimising the wasm module ==")
    print("   " + " ".join(str(c) for c in cmd))
    proc = subprocess.run([str(c) for c in cmd], cwd=str(RUST), capture_output=True, text=True)
    if proc.returncode != 0 or not out.exists():
        # A failed optimisation is not a failed build; say so and carry on.
        print(f"   wasm-opt failed ({proc.returncode}); using the unoptimised module")
        if proc.stderr.strip():
            print("   " + proc.stderr.strip().splitlines()[0])
        return wasm

    before, after = wasm.stat().st_size, out.stat().st_size
    print(f"   {before / 1024:.1f} KB -> {after / 1024:.1f} KB "
          f"({100 - after * 100 / before:.0f}% smaller)")
    return out


def find_wasm_opt():
    """Prefer one on PATH, else the copy trunk cached."""
    found = shutil.which("wasm-opt")
    if found:
        return Path(found)
    cache = Path(os.environ.get("LOCALAPPDATA", "")) / "trunkrs" / "trunk" / "cache"
    if cache.exists():
        candidates = sorted(cache.glob("wasm-opt-*/bin/wasm-opt.exe"), reverse=True)
        if candidates:
            return candidates[0]
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", action="store_true")
    ap.add_argument("--web", action="store_true")
    ap.add_argument("--debug", action="store_true")
    ap.add_argument("--run", action="store_true")
    ap.add_argument("--port", type=int, default=5050)
    args = ap.parse_args()

    both = not (args.server or args.web)
    binary = None
    if both or args.server:
        binary = build_server(release=not args.debug)
    if both or args.web:
        build_web()

    if args.run:
        if binary is None:
            binary = build_server(release=not args.debug)
        print(f"\n== serving on http://127.0.0.1:{args.port} ==")
        subprocess.run([
            str(binary),
            "--port", str(args.port),
            "--data-dir", str(RUST / "devdata"),
            "--static-dir", str(RUST / "static"),
        ], cwd=str(RUST))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
