#!/usr/bin/env python3
"""Reject reverse product dependencies and local paths outside the SDK."""
import argparse
import json
import subprocess
from urllib.parse import unquote
from pathlib import Path
import tomllib

FORBIDDEN = {"harness-runtime", "lingxi-llm-client", "llm-runtime", "platform-api", "client-protocol", "client-adapter", "branding", "tool-api", "protocol", "core", "agent", "orchestrator", "local-apps"}


def validate(root):
    root = Path(root).resolve()
    manifests = [root / "Cargo.toml", *sorted((root / "crates").glob("**/Cargo.toml"))]
    if len(manifests) < 2:
        raise ValueError("no SDK crate manifests discovered")
    def walk(value, directory):
        if not isinstance(value, dict):
            return
        for key, item in value.items():
            if key in {"dependencies", "dev-dependencies", "build-dependencies"}:
                for name, spec in item.items():
                    package = spec.get("package", name) if isinstance(spec, dict) else name
                    if package in FORBIDDEN:
                        raise ValueError(f"SDK has reverse product dependency: {package}")
                    if isinstance(spec, dict):
                        if any(token in spec.get("git", "").lower() for token in ("harness-runtime", "lingxi-next")):
                            raise ValueError("SDK may not fetch a product repository")
                        if "path" in spec:
                            path = (directory / spec["path"]).resolve()
                            if root not in path.parents or not (path / "Cargo.toml").is_file():
                                raise ValueError(f"SDK dependency escapes source closure: {path}")
            if isinstance(item, dict):
                walk(item, directory)
    for path in manifests:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
        walk(data, path.parent)
    print(f"SDK dependency boundary verified: {len(manifests)} manifests")


def validate_metadata(metadata, root):
    """Inspect all resolved packages and every declared conditional/build/dev edge."""
    root = Path(root).resolve()
    packages = metadata.get("packages")
    if not isinstance(packages, list) or not packages or not metadata.get("resolve"):
        raise ValueError("complete resolved Cargo metadata is required")
    by_id = {package["id"]:package for package in packages}
    if len(by_id) != len(packages):
        raise ValueError("duplicate Cargo package identity")
    def source_allowed(source):
        if any(token in unquote(source or "").lower() for token in ("harness-runtime", "lingxi-next")):
            raise ValueError("resolved SDK graph fetches a product repository")
    def contained(path):
        return Path(path).resolve().is_relative_to(root)
    for package in packages:
        if package["name"] in FORBIDDEN:
            raise ValueError(f"resolved reverse product dependency: {package['name']}")
        source_allowed(package.get("source"))
        if package.get("source") is None and not contained(package["manifest_path"]):
            raise ValueError("resolved path package escapes SDK source closure")
        for dependency in package.get("dependencies", []):
            if dependency["name"] in FORBIDDEN:
                raise ValueError("declared transitive/conditional reverse product dependency")
            source_allowed(dependency.get("source"))
            if dependency.get("path") and not contained(dependency["path"]):
                # A Git package's internal path edge must resolve to the exact
                # same Git source identity; arbitrary external path packages
                # and unresolved external conditional path edges fail closed.
                matches = [candidate for candidate in packages if Path(candidate["manifest_path"]).resolve().parent == Path(dependency["path"]).resolve()]
                if not package.get("source", "") or not str(package["source"]).startswith("git+") or not matches or any(candidate.get("source") != package["source"] for candidate in matches):
                    raise ValueError("declared path edge escapes resolved source closure")
    members = metadata.get("workspace_members", [])
    if not members or any(member not in by_id or by_id[member].get("source") is not None for member in members):
        raise ValueError("workspace members must be SDK-owned path packages")
    for node in metadata["resolve"].get("nodes", []):
        if node["id"] not in by_id or any(dep["pkg"] not in by_id for dep in node.get("deps", [])):
            raise ValueError("Cargo resolution references missing package identities")
    print(f"resolved SDK dependency boundary verified: {len(packages)} packages, all declared edge kinds")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--resolved", action="store_true", help="inspect cargo metadata --locked --all-features including transitive packages")
    args = parser.parse_args()
    validate(args.root)
    if args.resolved:
        output = subprocess.check_output(["cargo", "metadata", "--locked", "--all-features", "--format-version=1"],cwd=args.root,text=True,encoding="utf-8")
        validate_metadata(json.loads(output),args.root)
