#!/usr/bin/env python3
"""Verify native-support artifact contents and the SDK inputs that produced them."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition, message):
    if not condition:
        raise SystemExit(message)



def verify_swift_imports(framework, slices):
    targets = [
        ('iphonesimulator-arm64', 'ios-arm64_x86_64-simulator', 'arm64-apple-ios-simulator', 'arm64-apple-ios18.0-simulator', 'iphonesimulator'),
        ('iphonesimulator-x86_64', 'ios-arm64_x86_64-simulator', 'x86_64-apple-ios-simulator', 'x86_64-apple-ios18.0-simulator', 'iphonesimulator'),
        ('iphoneos-arm64', 'ios-arm64', 'arm64-apple-ios', 'arm64-apple-ios18.0', 'iphoneos'),
    ]
    for slice_name, directory, module_arch, _, _ in targets:
        if slice_name not in slices:
            continue
        module = framework / directory / 'MobileLinuxNativeSupport.framework' / 'Modules' / 'MobileLinuxNativeSupport.swiftmodule'
        interface = module / (module_arch + '.swiftinterface')
        require(interface.is_file(), f'missing importable Swift interface for {slice_name}: {interface.name}')
    if sys.platform != 'darwin' or shutil.which('xcrun') is None:
        return
    with tempfile.TemporaryDirectory(prefix='mobile-linux-native-import-') as temp:
        root = Path(temp)
        probe = root / 'probe.swift'
        probe.write_text('import MobileLinuxNativeSupport\nlet guestHome = MobileLinuxNativeSupport.LXISHGuestPaths.home\n')
        for slice_name, directory, _, triple, platform in targets:
            if slice_name not in slices:
                continue
            sdk = subprocess.check_output(['xcrun', '--sdk', platform, '--show-sdk-path'], text=True).strip()
            result = subprocess.run([
                'xcrun', 'swiftc', '-typecheck', '-target', triple, '-sdk', sdk,
                '-module-cache-path', str(root / 'module-cache'),
                '-F', str(framework / directory), str(probe),
            ], text=True, capture_output=True)
            require(result.returncode == 0, f'Swift import failed for {slice_name}: {result.stderr.strip()}')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifact-dir', type=Path, required=True)
    args = parser.parse_args()
    artifact = args.artifact_dir.resolve()
    sdk = Path(__file__).resolve().parents[2]
    manifest = json.loads((artifact / 'native-support-manifest.json').read_text())
    require(manifest['source_manifest_sha256'] == digest(sdk / 'native/ios/sources.json'), 'native source pins changed')
    require(manifest['kind'] == 'native-support' and manifest['contains_rust'] is False, 'expected a native-support artifact without a Rust core')
    slices = set(manifest['slices'])
    simulator = {'iphonesimulator-arm64', 'iphonesimulator-x86_64'}
    require(slices in (simulator, simulator | {'iphoneos-arm64'}), 'unexpected or missing native slices')
    framework = artifact / 'MobileLinuxNativeSupport.xcframework'
    verify_swift_imports(framework, slices)
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
