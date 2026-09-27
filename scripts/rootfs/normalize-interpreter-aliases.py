#!/usr/bin/env python3
"""Normalize exactly two required interpreter aliases in a NEW staging tree."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
sys.dont_write_bytecode = True
from source_contract import external_output


def normalize(root, record):
    root = external_output(root)
    record = external_output(record, root)
    changes = []
    for relative in ("bin/sh", "usr/bin/python3"):
        alias = root / relative
        if not alias.is_symlink():
            if not alias.is_file():
                raise ValueError(f"required interpreter missing: {relative}")
            continue
        link = os.readlink(alias)
        if Path(link).is_absolute():
            raise ValueError("required interpreter alias must be relative")
        target = alias.resolve(strict=True)
        target_relative = target.relative_to(root).as_posix()
        if not target_relative.startswith(("bin/", "usr/bin/")) or target.is_symlink() or not stat.S_ISREG(target.stat().st_mode):
            raise ValueError("interpreter target must be an immutable regular binary")
        data = target.read_bytes()
        if data[:4] != b"\x7fELF" or not target.stat().st_mode & stat.S_IXUSR:
            raise ValueError("interpreter target must be an executable ELF")
        before = hashlib.sha256(data).hexdigest()
        alias.unlink()
        os.link(target, alias)
        after = hashlib.sha256(alias.read_bytes()).hexdigest()
        if before != after:
            raise ValueError("interpreter bytes changed during alias normalization")
        changes.append({"path":"/"+relative,"previous_symlink":link,"target":"/"+target_relative,"transformation":"symlink-to-ELF-hardlink","before_sha256":before,"after_sha256":after})
    record.parent.mkdir(parents=True, exist_ok=True)
    record.write_text(json.dumps({"schema_version":1,"scope":["/bin/sh","/usr/bin/python3"],"transformations":changes},indent=2)+"\n")
    print(f"normalized {len(changes)} bounded interpreter aliases; original archive unchanged")

if __name__ == "__main__":
    parser=argparse.ArgumentParser()
    parser.add_argument("--root",required=True)
    parser.add_argument("--record",required=True)
    args=parser.parse_args()
    normalize(args.root,args.record)
