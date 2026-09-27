#!/usr/bin/env python3
"""Verify complete rootfs inventory, SPDX licenses, archive bytes and allowlist."""
import argparse
import hashlib
import json
from pathlib import Path
import sys
sys.dont_write_bytecode = True
import rootfs_tool as tool

def verify_archive_inventory(archive, expected):
    """Compare every immutable payload byte, including hardlink aliases, without extraction."""
    actual = []
    with tool.open_tar_archive(Path(archive)) as tar:
        for member in tar.getmembers():
            path = "/" + tool.safe_member_path(member.name).as_posix()
            if member.isdir() or tool.is_writable_inventory_path(path):
                continue
            if member.issym():
                payload = member.linkname.encode("utf-8")
                digest, size, kind = hashlib.sha256(payload).hexdigest(), len(payload), "symlink"
            elif member.isreg() or member.islnk():
                stream = tar.extractfile(member)
                if stream is None:
                    raise ValueError(f"archive payload missing: {path}")
                hasher, size = hashlib.sha256(), 0
                with stream:
                    for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                        hasher.update(chunk)
                        size += len(chunk)
                digest, kind = hasher.hexdigest(), "regular-file"
            else:
                raise ValueError(f"unsupported archive inventory entry: {path}")
            actual.append({"path":path, "sha256":digest, "kind":kind, "size_bytes":size})
    if sorted(actual, key=lambda entry: entry["path"]) != expected:
        raise ValueError("archive immutable payload inventory differs from manifest")

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
        verify_archive_inventory(archive, inventory)
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
