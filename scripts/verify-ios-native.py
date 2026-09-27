#!/usr/bin/env python3
"""Verify native-support artifact contents and the SDK inputs that produced them."""
import argparse
import hashlib
import json
from pathlib import Path


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifact-dir', type=Path, required=True)
    args = parser.parse_args()
    artifact = args.artifact_dir.resolve()
    sdk = Path(__file__).resolve().parents[1]
    manifest = json.loads((artifact / 'native-support-manifest.json').read_text())
    require(manifest['source_manifest_sha256'] == digest(sdk / 'native/ios/sources.json'), 'native source pins changed')
    require(manifest['kind'] == 'native-support' and manifest['contains_rust'] is False, 'expected a native-support artifact without a Rust core')
    slices = set(manifest['slices'])
    simulator = {'iphonesimulator-arm64', 'iphonesimulator-x86_64'}
    require(slices in (simulator, simulator | {'iphoneos-arm64'}), 'unexpected or missing native slices')
    framework = artifact / 'MobileLinuxNativeSupport.xcframework'
    scopes = [framework, artifact / 'licenses', artifact / 'source-provenance']
    require(all(scope.is_dir() and any(scope.rglob('*')) for scope in scopes), 'missing native license or source provenance sidecars')
    actual_files = {str(path.relative_to(artifact)) for scope in scopes for path in scope.rglob('*') if path.is_file()}
    require(actual_files and actual_files == set(manifest['artifacts']), 'native framework file set differs from manifest')
    for name, expected in manifest['artifacts'].items():
        path = artifact / name
        require(path.resolve().is_relative_to(artifact), 'artifact path escapes output directory')
        require(digest(path) == expected, 'native artifact hash mismatch: ' + name)
    inputs = manifest['source_file_sha256']
    require(bool(inputs), 'native source input manifest is empty')
    for name, expected in inputs.items():
        path = sdk / name
        require(path.resolve().is_relative_to(sdk), 'source path escapes SDK directory')
        require(digest(path) == expected, 'native source input changed: ' + name)
    print('iOS native-support artifact verified')


if __name__ == '__main__':
    main()
