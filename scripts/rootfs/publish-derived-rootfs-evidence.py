#!/usr/bin/env python3
"""Publish evidence for an independently checked workspace-root derivation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

sys.dont_write_bytecode = True


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


parser = argparse.ArgumentParser()
parser.add_argument("--sdk-root", type=Path, required=True)
parser.add_argument("--archive", type=Path, required=True)
parser.add_argument("--abi", choices=("arm64-v8a", "x86_64"), required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
sdk = args.sdk_root.resolve(strict=True)
archive = args.archive.resolve(strict=True)
sys.path.insert(0, str(sdk / "scripts/rootfs"))
import rootfs_tool as tool
from source_contract import external_output

historical = sdk / "docs/mobile-linux/releases/3.24.2-user1" / args.abi
output = external_output(args.output, archive, historical)
if output.exists():
    raise SystemExit("output already exists; supply a fresh directory")
pins_path = sdk / "docs/toolchains/runtime-pins.json"
pins = json.loads(pins_path.read_text())
version = pins["rootfs_version"]
if version != "3.24.2-user2":
    raise SystemExit("current rootfs version is not the workspace-root successor")
base = json.loads((historical / "rootfs-manifest.json").read_text())
derivation = json.loads((archive.parent / "derivation-inputs.json").read_text())
derive_script = sdk / "scripts/rootfs/derive-workspace-rootfs.py"
if (derivation["abi"] != args.abi or derivation["historical_release"] != "3.24.2-user1"
        or derivation["historical_manifest_sha256"] != sha256(historical / "rootfs-manifest.json")
        or derivation["historical_archive_sha256"] != base["archive"]["sha256"]
        or derivation["derivation_script_sha256"] != sha256(derive_script)
        or derivation["archive_sha256"] != sha256(archive)
        or derivation["archive_size_bytes"] != archive.stat().st_size
        or archive.name != "rootfs.tar.gz"):
    raise SystemExit("workspace-root derivation receipt does not match source and archive")
closure = json.loads((historical / "apk-closure.json").read_text())
if closure["artifacts"] != pins["apk_artifacts"][args.abi]["artifacts"]:
    raise SystemExit("historical package closure differs from current source pins")
previous = json.loads((historical / "producer-inputs.json").read_text())
for key in ("version", "url", "sha256", "configure_args", "license", "build_packages", "builder_images"):
    if previous["node_provenance"][key] != pins["node_source"][key]:
        raise SystemExit(f"historical Node provenance differs from source pins: {key}")

manifest = base.copy()
manifest["rootfs_version"] = version
manifest["brand_profile"] = {
    "brand_id": tool.GUEST_PROFILE.brand_id,
    "linux_user": tool.GUEST_PROFILE.linux_user,
    "config_dir_name": tool.GUEST_PROFILE.config_dir_name,
}
manifest["writable_paths"] = sorted(tool.WRITABLE_ROOTS)
manifest["archive"] = {"filename": archive.name, "sha256": sha256(archive), "size_bytes": archive.stat().st_size}
spdx = json.loads((historical / "rootfs.spdx.json").read_text())
old_name = f"mobile-linux-runtime-{args.abi}-3.24.2-user1"
new_name = f"mobile-linux-runtime-{args.abi}-{version}"
if spdx["name"] != old_name or f"/{old_name}/" not in spdx["documentNamespace"]:
    raise SystemExit("unexpected historical SPDX identity")
spdx["name"] = new_name
spdx["documentNamespace"] = spdx["documentNamespace"].replace(f"/{old_name}/", f"/{new_name}/", 1)

output.mkdir(parents=True)
for name in ("rootfs-build.lock.json", "executable-allowlist.json", "apk-closure.json", "interpreter-alias-transformations.json"):
    shutil.copy2(historical / name, output / name)
(output / "rootfs-manifest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
(output / "rootfs.spdx.json").write_text(json.dumps(spdx, indent=2, ensure_ascii=False) + "\n")
provenance = {
    "schema_version": 1,
    "abi": args.abi,
    "architecture": previous["architecture"],
    "rootfs_version": version,
    "archive_sha256": sha256(archive),
    "source_toolchain_pins_sha256": sha256(pins_path),
    "apk_closure_sha256": previous["apk_closure_sha256"],
    "node_provenance": previous["node_provenance"],
    "producer_input_sha256": {
        "derive-workspace-rootfs.py": sha256(derive_script),
        "publish-derived-rootfs-evidence.py": sha256(Path(__file__)),
        "verify-evidence.py": sha256(sdk / "scripts/rootfs/verify-evidence.py"),
    },
    "historical_producer_inputs_sha256": sha256(historical / "producer-inputs.json"),
    "workspace_root_derivation": derivation,
}
(output / "producer-inputs.json").write_text(json.dumps(provenance, indent=2, ensure_ascii=False) + "\n")
subprocess.run([
    sys.executable, str(sdk / "scripts/rootfs/verify-evidence.py"),
    "--evidence-dir", str(output), "--archive", str(archive),
], check=True)
environment = os.environ.copy()
environment["PATH"] = str(Path(sys.executable).parent) + os.pathsep + environment.get("PATH", "")
subprocess.run([
    "bash", str(sdk / "scripts/rootfs/check-rootfs-manifest.sh"), str(output / "rootfs-manifest.json"),
], check=True, env=environment)
print(f"published verified local {args.abi} evidence for {version}: {output}")
