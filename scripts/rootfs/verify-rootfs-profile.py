#!/usr/bin/env python3
"""Validate an explicit base or toolchain archive before native conversion."""
import argparse
import hashlib
import json
import stat
from pathlib import Path
import sys
sys.dont_write_bytecode = True
import rootfs_tool as tool
from source_contract import external_output


def verify(archive, arch, profile, expected_sha256, receipt=None):
    archive = Path(archive)
    if arch not in ("aarch64", "x86_64") or profile not in ("base", "toolchain"):
        raise ValueError("unsupported explicit rootfs profile/architecture")
    if profile == "base":
        pin = tool._PINS["alpine"]["minirootfs"][arch]["sha256"]
        if expected_sha256 != pin:
            raise ValueError("base archive digest differs from active SDK source pin")
    if len(expected_sha256) != 64 or any(c not in "0123456789abcdef" for c in expected_sha256):
        raise ValueError("explicit SHA256 required")
    if not archive.is_file() or archive.is_symlink() or tool.read_sha256(archive) != expected_sha256:
        raise ValueError("archive bytes differ from expected SHA256")
    if profile == "toolchain":
        tool.verify_archive(argparse.Namespace(archive=archive))
    machine = {"aarch64":183, "x86_64":62}[arch]
    inventory, elf_paths = [], []
    with tool.open_tar_archive(archive) as tar:
        members = tar.getmembers()
        paths = [tool.safe_member_path(member.name).as_posix() for member in members]
        if len(set(paths)) != len(paths):
            raise ValueError("duplicate archive path")
        if not {"bin/busybox", "bin/sh", "sbin/apk", "etc/apk/repositories"}.issubset(paths):
            raise ValueError("base archive missing required runtime paths")
        links = {path for path,member in zip(paths,members) if member.issym() or member.islnk()}
        for path in paths:
            if any(parent.as_posix() in links for parent in Path(path).parents):
                raise ValueError(f"archive entry traverses a link ancestor: {path}")
        for member in members:
            path = "/" + tool.safe_member_path(member.name).as_posix()
            if member.isdev() or member.isfifo() or not (member.isdir() or member.issym() or member.isreg() or member.islnk()):
                raise ValueError(f"unsafe archive entry type: {path}")
            if member.mode & (stat.S_ISUID | stat.S_ISGID):
                raise ValueError(f"privileged archive mode: {path}")
            if not member.issym() and member.mode & stat.S_IWOTH and path not in tool.WORLD_WRITABLE_ALLOWED:
                raise ValueError(f"unsafe writable archive entry: {path}")
            if member.issym():
                # Official base archives contain absolute *guest* links. Only
                # fakefs consumes these; never extract them into a host tree.
                target = member.linkname
                if target.startswith("/"):
                    resolved = tool.safe_member_path(target.lstrip("/")).as_posix()
                else:
                    resolved = tool.resolve_symlink_target(member.name, target).as_posix()
                if path.startswith(("/bin/", "/sbin/", "/usr/bin/", "/usr/sbin/")) and resolved not in paths:
                    raise ValueError(f"dangling executable link: {path}")
            if member.islnk():
                target = tool.safe_member_path(member.linkname).as_posix()
                if target not in paths or target in links:
                    raise ValueError(f"unsafe archive hardlink: {path}")
            if member.isdir():
                continue
            if member.issym():
                payload = member.linkname.encode("utf-8")
                digest, size, kind = hashlib.sha256(payload).hexdigest(), len(payload), "symlink"
            else:
                stream = tar.extractfile(member)
                if stream is None:
                    raise ValueError(f"archive entry has no payload: {path}")
                with stream:
                    header = stream.read(64)
                    hasher, size = hashlib.sha256(header), len(header)
                    for chunk in iter(lambda:stream.read(1024 * 1024), b""):
                        hasher.update(chunk); size += len(chunk)
                if header.startswith(b"\x7fELF"):
                    if len(header) < 20 or header[4] != 2 or header[5] not in (1,2):
                        raise ValueError(f"unsupported ELF header: {path}")
                    actual = int.from_bytes(header[18:20], "little" if header[5] == 1 else "big")
                    if actual != machine:
                        raise ValueError(f"ELF architecture differs from {arch}: {path}")
                    elf_paths.append(path)
                digest, kind = hasher.hexdigest(), "regular-file"
            inventory.append({"path":path,"kind":kind,"size_bytes":size,"sha256":digest})
    if "/bin/busybox" not in elf_paths:
        raise ValueError("archive needs a real architecture-checked BusyBox ELF")
    data = {"schema_version":1,"profile":profile,"architecture":arch,"archive_sha256":expected_sha256,
            "source_pins_sha256":tool.read_sha256(tool._PINS_PATH),"elf_paths":sorted(elf_paths),
            "inventory":sorted(inventory,key=lambda row:row["path"])}
    if receipt:
        output = external_output(receipt, archive)
        output.parent.mkdir(parents=True,exist_ok=True)
        output.write_text(json.dumps(data,indent=2)+"\n")
    print(f"rootfs {profile} profile verified: {arch}, {len(inventory)} entries, {len(elf_paths)} ELF paths")
    return data

if __name__ == "__main__":
    parser=argparse.ArgumentParser()
    parser.add_argument("--archive",required=True)
    parser.add_argument("--arch",choices=("aarch64","x86_64"),required=True)
    parser.add_argument("--profile",choices=("base","toolchain"),required=True)
    parser.add_argument("--sha256",required=True)
    parser.add_argument("--receipt")
    args=parser.parse_args()
    verify(args.archive,args.arch,args.profile,args.sha256,args.receipt)
