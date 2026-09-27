#!/usr/bin/env python3
"""Verify SDK-owned artifact identities; optionally verify every APK byte."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[2]
DEFAULT = ROOT / "docs/toolchains/runtime-pins.json"


def sha256(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError("missing or invalid SHA256")


def artifact(record, algorithm):
    if not str(record.get("url", "")).startswith("https://"):
        raise ValueError("artifact URL must use HTTPS")
    if algorithm == "sha256":
        sha256(record.get(algorithm))
    elif len(base64.b64decode(record.get(algorithm, ""), validate=True)) != 64:
        raise ValueError("artifact must have a complete SHA512")


def verify(path=DEFAULT, apk_dir=None):
    pins = json.loads(Path(path).read_text())
    if "local_app_runtime" in pins:
        raise ValueError("product profile must be supplied by caller, not SDK pins")
    count = 0
    for abi, arch in (("arm64-v8a", "aarch64"), ("x86_64", "x86_64")):
        artifact(pins["alpine"]["minirootfs"][arch], "sha256")
        records = pins["apk_artifacts"][abi]
        if records["alpine_arch"] != arch or not records["artifacts"]:
            raise ValueError("empty or wrong-architecture APK closure")
        found = {}
        for item in records["artifacts"]:
            artifact(item, "sha256")
            if item["arch"] not in (arch, "noarch") or item["name"] in found:
                raise ValueError("duplicate or wrong-architecture APK")
            found[item["name"]] = item["version"]
            if apk_dir:
                file = Path(apk_dir) / abi / (item["name"] + "-" + item["version"] + ".apk")
                if not file.is_file() or hashlib.sha256(file.read_bytes()).hexdigest() != item["sha256"]:
                    raise ValueError(f"missing or corrupt APK: {file}")
            count += 1
        for name, version in pins["runtime_packages"].items():
            if found.get(name) != version:
                raise ValueError(f"runtime package closure mismatch: {abi}/{name}")
        if set(found) & set(pins["forbidden_packages"]):
            raise ValueError("forbidden package in APK closure")
        for section in ("pnpm", "typescript_native"):
            artifact(pins[section]["packages"][arch], "sha512")
        sha256(pins["typescript_native"]["packages"][arch]["tsc_sha256"])
        image = pins["node_source"]["builder_images"][arch]
        if "@sha256:" not in image:
            raise ValueError("builder image must be immutable")
        sha256(image.rsplit("@sha256:", 1)[1])
    artifact(pins["node_source"], "sha256")
    for section in ("npm", "pnpm"):
        artifact(pins[section], "sha512")
    if not pins["node_source"].get("license") or not pins["typescript_native"].get("license"):
        raise ValueError("toolchain licenses required")
    print(f"SDK artifact pins verified: {count} APK identities, both architectures, Node/npm/pnpm/TypeScript")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--pins", type=Path, default=DEFAULT)
    parser.add_argument("--apk-dir", type=Path)
    args = parser.parse_args()
    verify(args.pins, args.apk_dir)
