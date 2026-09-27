#!/usr/bin/env python3
"""Reject reverse product dependencies and local paths outside the SDK."""
import argparse
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
        data = tomllib.loads(path.read_text())
        walk(data, path.parent)
    print(f"SDK dependency boundary verified: {len(manifests)} manifests")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    validate(parser.parse_args().root)
