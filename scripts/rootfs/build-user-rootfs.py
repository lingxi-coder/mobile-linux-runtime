#!/usr/bin/env python3
"""Add the default account to an SDK-attested toolchain image without rebuilding unchanged toolchains."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
sys.dont_write_bytecode = True
import guest_user
import rootfs_tool as tool
from source_contract import external_output


def build(base, abi, output, cache, container='docker'):
    arch = {'arm64-v8a': 'aarch64', 'x86_64': 'x86_64'}[abi]
    sdk = Path(__file__).resolve().parents[2]
    historical = sdk / 'docs/mobile-linux/releases' / tool._PINS['alpine']['version'] / abi
    manifest = json.loads((historical / 'rootfs-manifest.json').read_text())
    if base.is_symlink() or not base.is_file() or base.stat().st_size != manifest['archive']['size_bytes'] or tool.read_sha256(base) != manifest['archive']['sha256']:
        raise ValueError('base image differs from immutable SDK historical release')
    tool.verify_archive(argparse.Namespace(archive=base))
    output = external_output(output, base, cache)
    output.mkdir(parents=True, exist_ok=True)
    tree = output / 'tree'
    if tree.exists(): raise ValueError('output tree already exists; supply a fresh output directory')
    cache.mkdir(parents=True, exist_ok=True)
    records = tool._PINS['guest_user_packages'][arch]
    for record in records:
        with guest_user.apk_payload(record, cache): pass
    image = tool._PINS['node_source']['builder_images'][arch]
    packages = ['/apk/' + Path(record['url']).name for record in records]
    subprocess.run([container, 'run', '--rm', '--platform', 'linux/arm64' if arch == 'aarch64' else 'linux/amd64',
        '-v', str(base.resolve()) + ':/base.tar.gz:ro', '-v', str(output) + ':/out',
        '-v', str(cache.resolve()) + ':/apk:ro', image, '/bin/sh', '-eu', '-c',
        'mkdir /out/tree; tar -xzf /base.tar.gz -C /out/tree; apk --root /out/tree --no-network --allow-untrusted add "$@"',
        'guest-user-install', *packages], check=True)
    guest_user.apply_tree(tree)
    legacy_workspace = tree / 'workspace'
    if legacy_workspace.exists(): legacy_workspace.rmdir()
    archive = output / 'rootfs.tar.gz'
    tool.build_archive(argparse.Namespace(root=tree, output=output / 'rootfs.tar', source_date_epoch=0))
    with archive.open('wb') as file:
        subprocess.run(['gzip', '-n', '-9', '-c', str(output / 'rootfs.tar')], stdout=file, check=True)
    (output / 'rootfs.tar').unlink()
    closure = output / 'closure.json'
    closure.write_text(json.dumps({'arch': arch, 'artifacts': tool._PINS['apk_artifacts'][abi]['artifacts']}))
    evidence = output / 'evidence'
    subprocess.run([sys.executable, str(sdk / 'scripts/rootfs/publish-rootfs-evidence.py'), '--root', str(tree),
        '--archive', str(archive), '--closure', str(closure), '--abi', abi, '--output-dir', str(evidence)], check=True)
    shutil.copy2(historical / 'interpreter-alias-transformations.json', evidence)
    provenance = evidence / 'producer-inputs.json'
    data = json.loads(provenance.read_text())
    data['account_layer_base_archive_sha256'] = tool.read_sha256(base)
    data['account_layer_method'] = 'pinned offline APK installation over immutable SDK toolchain image'
    provenance.write_text(json.dumps(data, indent=2) + '\n')
    return archive


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base-archive', type=Path, required=True)
    parser.add_argument('--abi', choices=['arm64-v8a', 'x86_64'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cache', type=Path, required=True)
    parser.add_argument('--container-runtime', default='docker')
    args = parser.parse_args()
    print(build(args.base_archive.resolve(), args.abi, args.output.resolve(), args.cache.resolve(), args.container_runtime))
