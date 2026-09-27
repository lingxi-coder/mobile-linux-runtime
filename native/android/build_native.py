#!/usr/bin/env python3
"""Build pinned Android native support without a host app or writable SDK source."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile

SDK = Path(__file__).resolve().parents[2]
NATIVE = SDK / "native/android"
sys.dont_write_bytecode = True
sys.path.insert(0, str(SDK / "scripts"))
from sdk_artifact_identity import source_identity, file_hashes


def source_input_hashes():
    inputs = {}
    roots = [SDK / name for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
        "docs/android/native-pins.json", "native/android", "scripts/build-android-native.sh",
        "scripts/verify-android-native.py", "scripts/sdk_artifact_identity.py",
        "crates/platform-android-minijail", "crates/platform-android-libcap",
        "crates/platform-android-shellbin", "third_party/minijail", "third_party/libcap",
        "third_party/mksh", "third_party/toybox")]
    for root in roots:
        paths = root.rglob("*") if root.is_dir() else [root]
        for path in paths:
            if any(part in {".git", "__pycache__", "target"} for part in path.parts):
                continue
            if path.is_symlink():
                raise ValueError("SDK native source symlinks are not supported: " + str(path))
            if path.is_file(): inputs[path.relative_to(SDK).as_posix()] = digest(path)
    return dict(sorted(inputs.items()))


def run(args, **kwargs):
    print("+ " + " ".join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), check=True, **kwargs)

def git(source, *args):
    return subprocess.check_output(["git", "-C", str(source), *args])

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ndk", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--cache-dir", required=True, type=Path)
    parser.add_argument("--proot-source", type=Path, help="read-only pinned PRoot Git checkout")
    parser.add_argument("--abi", choices=("all", "arm64-v8a", "x86_64"), default="all")
    parser.add_argument("--android-api", type=int, default=26)
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--without-legacy-shell", action="store_true", help="omit optional mksh/toybox/PTY bridge")
    args = parser.parse_args()
    if args.android_api < 26 or args.jobs < 1:
        parser.error("android-api must be >=26 and jobs must be positive")
    output, cache = args.output_dir.resolve(), args.cache_dir.resolve()
    for path in (output, cache):
        if path == SDK or (path.is_relative_to(SDK) and not path.is_relative_to(SDK / "build")):
            parser.error("output/cache must be external or beneath SDK build/")
        path.mkdir(parents=True, exist_ok=True)
    source_revision, source_dirty = source_identity(SDK)
    source_inputs = source_input_hashes()
    pins = json.loads((SDK / "docs/android/native-pins.json").read_text())
    for relative, expected in pins["sdk_sources"].items():
        if digest(NATIVE / relative) != expected:
            raise SystemExit("Android source SHA-256 mismatch: " + relative)
    pin = pins["components"]["proot"]
    source = args.proot_source.resolve() if args.proot_source else cache / "sources/proot"
    if args.proot_source:
        actual_root = Path(git(source, "rev-parse", "--show-toplevel").decode().strip()).resolve()
        if actual_root != source or git(source, "rev-parse", "HEAD").decode().strip() != pin["commit"]:
            raise SystemExit("--proot-source must be the independent pinned PRoot checkout")
    elif not (source / "HEAD").exists():
        source.parent.mkdir(parents=True, exist_ok=True)
        run(["git", "init", "--bare", source])
        run(["git", "-C", source, "fetch", "--depth=1", pin["repository"], pin["commit"]])
    archive = git(source, "archive", "--format=tar", pin["commit"])
    if hashlib.sha256(archive).hexdigest() != pin["git_archive_sha256"]:
        raise SystemExit("PRoot pinned Git archive SHA-256 mismatch")
    license_dir = output / "licenses"
    license_dir.mkdir(exist_ok=True)
    license_records = []
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        proot_license = tar.extractfile("COPYING").read()
    for filename, component, data in [
        ("GPL-3.0-only.txt", "openminis", (NATIVE / "OPENMINIS-LICENSE").read_bytes()),
        ("GPL-2.0-or-later.txt", "proot", proot_license),
    ]:
        expected = pins["components"][component]["license_file_sha256"]
        if hashlib.sha256(data).hexdigest() != expected:
            raise SystemExit("pinned source license SHA-256 mismatch: " + component)
        path = license_dir / filename
        path.write_bytes(data)
        license_records.append({"path": "licenses/" + filename, "sha256": expected, "size_bytes": len(data), "component": component})
    for component, record in pins["distribution_licenses"].items():
        data = (SDK / record["source"]).read_bytes()
        if hashlib.sha256(data).hexdigest() != record["sha256"]:
            raise SystemExit("pinned distribution license mismatch: " + component)
        (license_dir / record["filename"]).write_bytes(data)
        license_records.append({"path": "licenses/" + record["filename"], "sha256": record["sha256"], "size_bytes": len(data), "component": component})
    host_tag = "darwin-x86_64" if platform.system() == "Darwin" else "linux-x86_64"
    tools = args.ndk.resolve() / "toolchains/llvm/prebuilt" / host_tag / "bin"
    if not (tools / "llvm-strip").is_file():
        raise SystemExit("NDK LLVM tools missing: " + str(tools))
    abis = ["arm64-v8a", "x86_64"] if args.abi == "all" else [args.abi]
    env = os.environ.copy()
    env.update(ANDROID_NDK_HOME=str(args.ndk.resolve()), CARGO_TARGET_DIR=str(cache / "cargo"), CARGO_INCREMENTAL="0")
    env["RUSTUP_TOOLCHAIN"] = subprocess.check_output(["rustup", "show", "active-toolchain"], cwd=SDK, text=True).split()[0]
    # PRoot's Makefile invokes readelf without a configurable variable.
    host_tools = cache / "host-tools"
    host_tools.mkdir(exist_ok=True)
    readelf = host_tools / "readelf"
    if readelf.is_symlink(): readelf.unlink()
    if not readelf.exists(): readelf.symlink_to(tools / "llvm-readelf")
    env["PATH"] = str(host_tools) + os.pathsep + str(tools) + os.pathsep + env["PATH"]
    artifacts = []
    for abi in abis:
        triple, machine = ("aarch64-linux-android", 183) if abi == "arm64-v8a" else ("x86_64-linux-android", 62)
        cc = tools / f"{triple}{args.android_api}-clang"
        with tempfile.TemporaryDirectory(prefix=f"native-{abi}-", dir=cache) as temporary:
            work = Path(temporary)
            proot = work / "proot"
            proot.mkdir()
            with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
                tar.extractall(proot, filter="data")
            shutil.copy2(NATIVE / "proot_lingxi_network_policy.c", proot / "src/extension/port_switch/port_switch.c")
            talloc = work / "talloc"
            shutil.copytree(NATIVE / "talloc", talloc)
            run([cc, "-c", talloc / "talloc.c", "-o", talloc / "talloc.o", "-I" + str(talloc), "-fPIC", "-O2", "-std=gnu99", "-DHAVE_STDARG_H=1", "-DHAVE_VA_COPY=1", "-DHAVE_UNISTD_H=1", "-DHAVE_INTPTR_T=1"], env=env)
            run([tools / "llvm-ar", "rcs", talloc / "libtalloc.a", talloc / "talloc.o"], env=env)
            run(["make", f"CC={cc}", f"STRIP={tools / 'llvm-strip'}", f"OBJCOPY={tools / 'llvm-objcopy'}", f"OBJDUMP={tools / 'llvm-objdump'}", "PROOT_UNBUNDLE_LOADER=/proc/self/fd", f"CPPFLAGS=-D_FILE_OFFSET_BITS=64 -D_GNU_SOURCE -I. -DARG_MAX=131072 -I{talloc}", 'CFLAGS=-O2 -Wall -Wextra -fPIE -DPROOT_UNBUNDLE_LOADER=\\\"/proc/self/fd\\\"', f"LDFLAGS=-Wl,-z,noexecstack -pie {talloc / 'libtalloc.a'}", f"-j{args.jobs}"], cwd=proot / "src", env=env)
            stage = work / "artifacts"
            stage.mkdir()
            shutil.copy2(proot / "src/proot", stage / "libproot.so")
            shutil.copy2(proot / "src/loader/loader", stage / "libproot-loader.so")
            run([cc, NATIVE / "mobile_linux_policy_launcher.c", "-o", stage / "libmobile_linux_policy_launcher.so", "-O2", "-Wall", "-Wextra", "-Werror", "-fPIE", "-pie", "-Wl,-z,noexecstack"], env=env)
            if not args.without_legacy_shell:
                run([cc, NATIVE / "pty_bridge.c", "-o", stage / "libpty_bridge.so", "-shared", "-fPIC", "-O2", "-llog"], env=env)
                run(["cargo", "ndk", "-t", abi, "--platform", "29", "build", "--locked", "--release", "-j", str(args.jobs), "-p", "platform-android-shellbin", "-p", "platform-android-minijail"], cwd=SDK, env=env)
                candidates = sorted((cache / "cargo" / triple / "release/build").glob("platform-android-shellbin-*/out"), key=lambda p: p.stat().st_mtime, reverse=True)
                shell_out = next((p for p in candidates if (p / "mksh").is_file() and (p / "toybox").is_file()), None)
                if shell_out is None: raise SystemExit("shellbin build produced no mksh/toybox")
                for name in ("mksh", "toybox"): shutil.copy2(shell_out / name, stage / f"lib{name}.so")
            for artifact in sorted(stage.iterdir()):
                run([tools / "llvm-strip", artifact], env=env)
                data = artifact.read_bytes()
                if data[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", data, 18)[0] != machine:
                    raise SystemExit("wrong ELF architecture: " + str(artifact))
                dest = output / "jniLibs" / abi / artifact.name
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(artifact, dest)
                artifacts.append({"path": dest.relative_to(output).as_posix(), "sha256": digest(dest), "size_bytes": dest.stat().st_size, "abi": abi})
    if source_identity(SDK) != (source_revision, source_dirty) or source_input_hashes() != source_inputs:
        raise SystemExit("SDK native build inputs changed during compilation; rebuild before publishing")
    manifest = {"source_revision": source_revision, "source_dirty": source_dirty,
        "source_inputs": source_inputs, "files": file_hashes(output, "native-manifest.json"),
        "schema_version": 1, "kind": "native-support-only", "contains_rust_core": False, "android_api": args.android_api, "legacy_shell_android_api": 29 if not args.without_legacy_shell else None, "legacy_host_shell": not args.without_legacy_shell, "proot_revision": pin["commit"], "source_pins_sha256": digest(SDK / "docs/android/native-pins.json"), "artifacts": artifacts, "licenses": license_records}
    (output / "native-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    run([sys.executable, SDK / "scripts/verify-android-native.py", "--artifact-dir", output])
    print("Android native support verified: " + str(output), flush=True)

if __name__ == "__main__": main()
