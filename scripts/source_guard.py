#!/usr/bin/env python3
"""Snapshot every source file, including untracked resources; only Git metadata is excluded."""
import argparse
import hashlib
import json
from pathlib import Path


def snapshot(root):
    result = {}
    for path in sorted(Path(root).rglob("*")):
        relative = path.relative_to(root)
        if ".git" in relative.parts:
            continue
        if path.is_symlink():
            result[relative.as_posix()] = "symlink:" + str(path.readlink())
        elif path.is_file():
            result[relative.as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--snapshot", type=Path, required=True)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if args.root.resolve() in args.snapshot.resolve().parents:
        raise SystemExit("snapshot output must be outside source checkout")
    current = snapshot(args.root)
    if args.check:
        previous = json.loads(args.snapshot.read_text())
        differences = sorted(k for k in current.keys() | previous.keys() if current.get(k) != previous.get(k))
        if differences:
            raise SystemExit("source checkout mutated: " + ", ".join(differences))
        print(f"source immutability verified: {len(current)} files")
    else:
        args.snapshot.write_text(json.dumps(current, sort_keys=True) + "\n")
