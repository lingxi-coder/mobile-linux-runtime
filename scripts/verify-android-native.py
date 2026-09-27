#!/usr/bin/env python3
"""Verify SDK-owned Android source pins or native-support-only build output."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import sys

sys.dont_write_bytecode = True
from sdk_artifact_identity import source_identity, validate_artifacts

SDK = Path(__file__).resolve().parents[1]

def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()
def fail(message): raise SystemExit(message)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    scope = parser.add_mutually_exclusive_group(required=True)
    scope.add_argument("--source-only", action="store_true")
    scope.add_argument("--artifact-dir", type=Path)
    args = parser.parse_args()
    pin_path = SDK / "docs/android/native-pins.json"
    pins = json.loads(pin_path.read_text())
    if pins.get("schema_version") != 1 or set(pins["components"]) != {"openminis", "proot", "talloc", "pty_bridge"}:
        fail("Android-only source pin schema/components diverged")
    sources = pins.get("sdk_sources", {})
    required_sources = {"mobile_linux_policy_launcher.c", "proot_lingxi_network_policy.c", "pty_bridge.c", "talloc/talloc.c", "talloc/talloc.h", "talloc/replace.h", "PtyBridge.kt", "patches/proot-loader-16k.patch"}
    if set(sources) != required_sources: fail("incomplete Android SDK source inventory")
    for relative, expected in sources.items():
        path = SDK / "native/android" / relative
        if path.is_symlink() or not path.is_file() or sha(path) != expected:
            fail("Android SDK source digest mismatch: " + relative)
    if sources["pty_bridge.c"] != pins["components"]["pty_bridge"]["sha256"]:
        fail("vendored PTY source differs from upstream pin")
    for path, expected in pins["components"]["talloc"]["openminis_vendored_files"].items():
        if sources[path.removeprefix("deps/")] != expected: fail("vendored talloc differs from pin")
    if sha(SDK / "native/android/OPENMINIS-LICENSE") != pins["components"]["openminis"]["license_file_sha256"]:
        fail("vendored OpenMinis license differs from pinned source")
    distribution = pins.get("distribution_licenses", {})
    required_licenses = {"talloc-license", "talloc-notice", "mksh", "toybox", "minijail-license", "minijail-notice", "libcap"}
    if set(distribution) != required_licenses: fail("incomplete distribution license source inventory")
    for component, record in distribution.items():
        path = SDK / record["source"]
        if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(SDK) or sha(path) != record["sha256"]:
            fail("distribution license source mismatch: " + component)
    if args.source_only:
        print("Android-only SDK source pins verified")
        return
    root = args.artifact_dir.resolve()
    revision, _dirty = source_identity(SDK)
    manifest = validate_artifacts(root, "native-manifest.json", revision, allow_dirty=True)
    # Source verification accepts explicitly marked development builds; release
    # publishers separately require both source and artifact cleanliness.
    inputs = manifest.get("source_inputs")
    if not isinstance(inputs, dict) or not inputs: fail("missing native build input inventory")
    for relative, expected in inputs.items():
        path = SDK / relative
        if not path.resolve().is_relative_to(SDK) or path.is_symlink() or not path.is_file() or sha(path) != expected:
            fail("SDK native build input changed: " + relative)
    if manifest.get("schema_version") != 1 or manifest.get("kind") != "native-support-only" or manifest.get("contains_rust_core") is not False:
        fail("native-support manifest must exclude Rust core/FFI")
    if manifest.get("source_pins_sha256") != sha(pin_path) or manifest.get("proot_revision") != pins["components"]["proot"]["commit"]:
        fail("native-support provenance differs from SDK pins")
    license_records = manifest.get("licenses", [])
    expected_licenses = {"openminis": "licenses/GPL-3.0-only.txt", "proot": "licenses/GPL-2.0-or-later.txt"}
    expected_licenses.update({key: "licenses/" + record["filename"] for key, record in distribution.items()})
    if len(license_records) != len(expected_licenses): fail("incomplete native license inventory")
    license_seen = set()
    for record in license_records:
        component = record.get("component")
        if component in license_seen or component not in expected_licenses or record.get("path") != expected_licenses[component]:
            fail("unexpected native license inventory")
        license_seen.add(component)
        path = root / record["path"]
        expected = distribution[component]["sha256"] if component in distribution else pins["components"][component]["license_file_sha256"]
        if path.is_symlink() or not path.is_file() or sha(path) != expected or record.get("sha256") != expected or path.stat().st_size != record.get("size_bytes"):
            fail("native source license digest mismatch: " + component)
    expected_names = {"libproot.so", "libproot-loader.so", "libmobile_linux_policy_launcher.so"}
    if manifest.get("legacy_host_shell") is True: expected_names |= {"libpty_bridge.so", "libmksh.so", "libtoybox.so"}
    records = manifest.get("artifacts", [])
    abis = {record.get("abi") for record in records}
    if not abis or not abis <= {"arm64-v8a", "x86_64"}: fail("invalid native ABI inventory")
    seen = set()
    for record in records:
        relative = record.get("path", "")
        if relative in seen: fail("duplicate native artifact: " + relative)
        seen.add(relative)
        path = root / relative
        if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(root): fail("unsafe native artifact: " + relative)
        if path.stat().st_size != record.get("size_bytes") or sha(path) != record.get("sha256"): fail("native digest mismatch: " + relative)
        data = path.read_bytes(); machine = 183 if record["abi"] == "arm64-v8a" else 62
        if data[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", data, 18)[0] != machine: fail("native ELF ABI mismatch: " + relative)
        if record["abi"] == "arm64-v8a":
            if len(data) < 64: fail("truncated ARM64 ELF header: " + relative)
            phoff = struct.unpack_from("<Q", data, 32)[0]
            phsize, phcount = struct.unpack_from("<HH", data, 54)
            if phsize < 56 or not phcount or phoff + phsize * phcount > len(data):
                fail("invalid ARM64 ELF program headers: " + relative)
            loads = [struct.unpack_from("<Q", data, phoff + index * phsize + 48)[0]
                     for index in range(phcount)
                     if struct.unpack_from("<I", data, phoff + index * phsize)[0] == 1]
            if not loads or min(loads) < 16384:
                fail("Android ARM64 helper lacks 16 KiB page alignment: " + relative)
    expected_paths = {f"jniLibs/{abi}/{name}" for abi in abis for name in expected_names}
    actual_paths = {p.relative_to(root).as_posix() for p in (root / "jniLibs").rglob("*") if p.is_file() or p.is_symlink()}
    if seen != expected_paths or actual_paths != expected_paths: fail("native-support artifact set differs from declared features")
    print(f"Android native-support verified: {len(records)} artifacts, {len(abis)} ABIs, no Rust core")

if __name__ == "__main__": main()
