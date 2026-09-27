#!/usr/bin/env python3
"""Verify complete rootfs inventory, SPDX licenses, archive bytes and allowlist."""
import argparse
import hashlib
import json
from pathlib import Path
import sys
sys.dont_write_bytecode = True
import rootfs_tool as tool

def verify(directory, archive=None, root=None):
    directory = Path(directory)
    manifest = json.loads((directory / "rootfs-manifest.json").read_text())
    spdx = json.loads((directory / "rootfs.spdx.json").read_text())
    allowlist = json.loads((directory / "executable-allowlist.json").read_text())
    tool.validate_lock(argparse.Namespace(lock=directory / "rootfs-build.lock.json", manifest=directory / "rootfs-manifest.json"))
    if not str(spdx.get("spdxVersion", "")).startswith("SPDX-2."):
        raise ValueError("SPDX document required")
    expected = sorted((p["name"], p["version"], p["license"]) for p in manifest["packages"])
    actual = sorted((p["name"], p["versionInfo"], p["licenseDeclared"]) for p in spdx["packages"])
    if expected != actual or any(not p[2] for p in actual):
        raise ValueError("SPDX package/license inventory differs from rootfs")
    if allowlist != {"schema_version":1, "entries":manifest["executable_allowlist"]}:
        raise ValueError("executable allowlist inventory differs from rootfs")
    inventory = manifest["immutable_files"]
    if hashlib.sha256(tool.canonical_json_bytes(inventory)).hexdigest() != manifest["content_sha256"]:
        raise ValueError("immutable content inventory digest mismatch")
    if archive:
        archive = Path(archive)
        if archive.stat().st_size != manifest["archive"]["size_bytes"] or tool.read_sha256(archive) != manifest["archive"]["sha256"]:
            raise ValueError("release archive bytes do not match manifest")
        tool.verify_archive(argparse.Namespace(archive=archive))
    if root:
        root = Path(root)
        tool.validate_rootfs_tree(root)
        if tool.collect_immutable_files(root) != inventory or tool.collect_allowlist(root) != manifest["executable_allowlist"]:
            raise ValueError("actual rootfs differs from complete inventory")
    print(f"rootfs evidence verified: {len(actual)} licensed packages, {len(inventory)} immutable entries")

if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--evidence-dir", required=True)
    parser.add_argument("--archive")
    parser.add_argument("--root")
    args = parser.parse_args()
    verify(args.evidence_dir, args.archive, args.root)
