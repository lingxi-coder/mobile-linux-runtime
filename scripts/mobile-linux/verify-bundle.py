#!/usr/bin/env python3
"""Validate a caller-provided dependency bundle without knowing any product profile."""
import argparse
import sys
sys.dont_write_bytecode = True
import hashlib
from pathlib import Path
import re
from source_contract import external_output


def verify(bundle, digest, output, cache):
    bundle = Path(bundle).resolve(strict=True)
    if not bundle.is_dir() or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError("bundle directory and explicit SHA256 are required")
    for p in bundle.rglob("*"):
        if p.is_symlink():
            raise ValueError(f"dependency bundle may not contain symlinks: {p}")
    for name in ("package.json", "pnpm-lock.yaml"):
        if not (bundle / name).is_file():
            raise ValueError(f"missing dependency input: {name}")
    actual = hashlib.sha256((bundle / "pnpm-lock.yaml").read_bytes()).hexdigest()
    if actual != digest:
        raise ValueError("explicit bundle lock digest mismatch")
    out = external_output(output, bundle)
    cache = external_output(cache, bundle)
    if out == cache or out in cache.parents or cache in out.parents:
        raise ValueError("output and cache must not overlap")
    print(f"bundle lock verified: {actual}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--bundle-dir", required=True)
    parser.add_argument("--lock-sha256", required=True)
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--cache-dir", required=True)
    args = parser.parse_args()
    verify(args.bundle_dir, args.lock_sha256, args.output_dir, args.cache_dir)
