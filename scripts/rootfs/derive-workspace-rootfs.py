#!/usr/bin/env python3
"""Derive a rootfs archive without extracting or altering privileged modes."""
import argparse
import copy
import contextlib
import gzip
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

sys.dont_write_bytecode = True


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def stream_sha256(stream):
    digest = hashlib.sha256()
    for block in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(block)
    return digest.hexdigest()


parser = argparse.ArgumentParser()
parser.add_argument("--sdk-root", type=Path, required=True)
parser.add_argument("--archive", type=Path, required=True)
parser.add_argument("--abi", choices=("arm64-v8a", "x86_64"), required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
sdk = args.sdk_root.resolve(strict=True)
archive = args.archive.resolve(strict=True)
evidence = sdk / "docs/mobile-linux/releases/3.24.2-user1" / args.abi
manifest_path = evidence / "rootfs-manifest.json"
sys.path.insert(0, str(sdk / "scripts/rootfs"))
from source_contract import external_output

output = external_output(args.output, archive, evidence)
if output.exists():
    raise SystemExit("output already exists; supply a fresh directory")
manifest = json.loads(manifest_path.read_text())
record = manifest["archive"]
digest = sha256(archive)
if archive.name != record["filename"] or archive.stat().st_size != record["size_bytes"] or digest != record["sha256"]:
    raise SystemExit("source archive differs from immutable historical manifest")
# The fixed historical input predates brand_profile and uses the former
# writable root. Adapt only an ephemeral copy for inventory verification;
# neither the old evidence nor this copy is accepted as a current release.
if manifest.get("brand_profile") is not None or set(manifest["writable_paths"]) != {
    "/home/lingxi", "/home/lingxi/lingxi", "/root", "/tmp", "/var/tmp"
}:
    raise SystemExit("historical manifest does not match the pinned input contract")
adapted = copy.deepcopy(manifest)
adapted["brand_profile"] = {"brand_id": "lingxi", "linux_user": "lingxi", "config_dir_name": ".lingxi"}
adapted["writable_paths"] = sorted({"/home/lingxi", "/home/lingxi/.lingxi", "/root", "/tmp", "/var/tmp"})
with tempfile.TemporaryDirectory(prefix="historical-rootfs-evidence-") as temporary:
    temporary = Path(temporary)
    for name in ("rootfs-build.lock.json", "rootfs.spdx.json", "executable-allowlist.json"):
        shutil.copy2(evidence / name, temporary / name)
    (temporary / "rootfs-manifest.json").write_text(json.dumps(adapted))
    subprocess.run([
        sys.executable, str(sdk / "scripts/rootfs/verify-evidence.py"),
        "--evidence-dir", str(temporary), "--archive", str(archive),
    ], check=True)

old = "home/lingxi/lingxi"
home = "home/lingxi"
added = ("home/lingxi/.lingxi", "home/lingxi/.lingxi/projects")
with tarfile.open(archive, "r:gz") as source:
    members = source.getmembers()
    names = [member.name.rstrip("/") for member in members]
    if len(names) != len(set(names)):
        raise SystemExit("source archive contains duplicate entries")
    if names.count(home) != 1 or names.count(old) != 1:
        raise SystemExit("expected exactly one historical home and workspace")
    if not members[names.index(old)].isdir() or any(name.startswith(old + "/") for name in names):
        raise SystemExit("historical workspace is not an empty directory")
    if any(name == path or name.startswith(path + "/") for name in names for path in added):
        raise SystemExit("source already contains a new workspace entry")
    output.mkdir(parents=True)
    target_path = output / "rootfs.tar.gz"
    with target_path.open("wb") as target, gzip.GzipFile(filename="", mode="wb", compresslevel=9, fileobj=target, mtime=0) as zipped:
        with tarfile.open(fileobj=zipped, mode="w|", format=tarfile.PAX_FORMAT) as dest:
            for member in members:
                name = member.name.rstrip("/")
                if name == old:
                    continue
                payload = source.extractfile(member) if member.isreg() else None
                with payload if payload is not None else contextlib.nullcontext() as stream:
                    dest.addfile(copy.copy(member), stream)
                if name == home:
                    for path in added:
                        entry = tarfile.TarInfo(path)
                        entry.type = tarfile.DIRTYPE
                        entry.mode = 0o755
                        entry.uid = entry.gid = 1000
                        entry.uname = entry.gname = "lingxi"
                        entry.mtime = 0
                        dest.addfile(entry)

subprocess.run([
    sys.executable, str(sdk / "scripts/rootfs/rootfs_tool.py"),
    "verify-archive", "--archive", str(target_path),
], check=True)
with tarfile.open(archive, "r:gz") as source, tarfile.open(target_path, "r:gz") as target:
    source_members = {item.name.rstrip("/"): item for item in source.getmembers()}
    target_members = {item.name.rstrip("/"): item for item in target.getmembers()}
    if set(target_members) != (set(source_members) - {old}) | set(added):
        raise SystemExit("derived archive has unexpected members")
    fields = ("type", "mode", "uid", "gid", "uname", "gname", "size", "mtime", "linkname", "pax_headers")
    for name, before in source_members.items():
        if name == old:
            continue
        after = target_members[name]
        if any(getattr(before, field) != getattr(after, field) for field in fields):
            raise SystemExit(f"archive metadata changed: {name}")
        if before.isreg():
            with source.extractfile(before) as before_file, target.extractfile(after) as after_file:
                if stream_sha256(before_file) != stream_sha256(after_file):
                    raise SystemExit(f"archive payload changed: {name}")
    for name in added:
        item = target_members[name]
        if not item.isdir() or (item.mode, item.uid, item.gid, item.uname, item.gname) != (0o755, 1000, 1000, "lingxi", "lingxi"):
            raise SystemExit(f"new workspace directory metadata is invalid: {name}")

provenance = {
    "schema_version": 1,
    "abi": args.abi,
    "historical_release": "3.24.2-user1",
    "historical_archive_sha256": digest,
    "historical_archive_size_bytes": record["size_bytes"],
    "historical_manifest_sha256": sha256(manifest_path),
    "derivation_script_sha256": sha256(Path(__file__)),
    "archive_sha256": sha256(target_path),
    "archive_size_bytes": target_path.stat().st_size,
    "removed_directory": old,
    "added_directories": list(added),
    "other_members_verified_unchanged": len(source_members) - 1,
}
(output / "derivation-inputs.json").write_text(json.dumps(provenance, indent=2) + "\n")
print(f"verified {args.abi} rootfs archive: {target_path} ({provenance['archive_sha256']})")
