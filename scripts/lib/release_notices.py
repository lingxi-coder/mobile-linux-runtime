"""Stage the SDK, PTY and all locked Rust registry license texts."""
from pathlib import Path
import hashlib
import shutil
import json
import tomllib

ROOT = Path(__file__).resolve().parents[2]
REGISTRY_MANIFEST = 'docs/licenses/rust/registry-manifest.json'
FILES = {'SDK-LICENSE': 'LICENSE', 'SDK-LICENSE-APACHE': 'LICENSE-APACHE', 'platform-pty-NOTICE': 'crates/platform-pty/NOTICE',
         'platform-pty-LICENSE-APACHE': 'crates/platform-pty/LICENSE-APACHE',
         'COMPONENT-NOTICE.md': 'docs/mobile-linux/LICENSES/NOTICE.md',
         'rust/overrides.json': 'docs/licenses/rust/overrides.json'}


def registry_files(root):
    data = json.loads((root / REGISTRY_MANIFEST).read_text())
    if data['schema_version'] != 1:
        raise ValueError('unsupported Rust license manifest')
    locked = {(p['name'], p['version']): p for p in tomllib.loads((root / 'Cargo.lock').read_text())['package']
              if (p.get('source') or '').startswith('registry+')}
    entries = {(p['name'], p['version']): p for p in data['packages']}
    if len(entries) != len(data['packages']) or set(entries) != set(locked):
        raise ValueError('Rust license inventory differs from Cargo.lock')
    overrides = json.loads((root / 'docs/licenses/rust/overrides.json').read_text())
    if overrides['schema_version'] != 1:
        raise ValueError('unsupported Rust license override manifest')
    by_override = {}
    for item in overrides['entries']:
        identity = (item['name'], item['version'])
        by_override.setdefault(identity, []).append(item)
        if identity not in entries or not entries[identity]['override']:
            raise ValueError('Rust license override has no matching registry package')
        if item.get('source_package'):
            source = locked.get((item['source_package'], item['source_package_version']))
            if source is None or source['checksum'] != item['source_package_checksum']:
                raise ValueError('Rust license override source package differs from Cargo.lock')
        license_path = root / item['license_path']
        if hashlib.sha256(license_path.read_bytes()).hexdigest() != item['license_sha256']:
            raise ValueError('Rust license override text differs from committed digest')
    result = {'rust/registry-manifest.json': REGISTRY_MANIFEST}
    for identity, entry in entries.items():
        lock = locked[identity]
        if entry['checksum'] != lock['checksum'] or entry['source'] != lock['source'] or not entry['license'] or not entry['files']:
            raise ValueError('Rust license package identity differs from Cargo.lock: ' + repr(identity))
        if entry['override'] and {item['license_sha256'] for item in by_override.get(identity, [])} != {item['sha256'] for item in entry['files']}:
            raise ValueError('Rust license override attribution differs from copied text: ' + repr(identity))
        for item in entry['files']:
            source = Path(item['path'])
            if not source.as_posix().startswith('docs/licenses/rust/files/') or '..' in source.parts:
                raise ValueError('Rust license path escapes committed inventory')
            payload = (root / source).read_bytes()
            if hashlib.sha256(payload).hexdigest() != item['sha256']:
                raise ValueError('Rust license text differs from committed digest: ' + str(source))
            target = 'rust/' + source.name
            if target in result:
                raise ValueError('duplicate Rust license destination: ' + target)
            result[target] = source.as_posix()
    return result


FILES.update(registry_files(ROOT))

def stage(root, destination):
    destination.mkdir(parents=True, exist_ok=True)
    for name, source in FILES.items():
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / source, target)

def verify(root, directory):
    # Re-evaluate against the current lock, even if the module was loaded before
    # a dependency edit in the same Python process.
    if registry_files(root) != {k:v for k,v in FILES.items() if k.startswith('rust/') and k != 'rust/overrides.json'}:
        raise ValueError('Rust license inventory changed after module load')
    for name, source in FILES.items():
        if not (directory/name).is_file() or (directory/name).read_bytes() != (root/source).read_bytes():
            raise ValueError('missing or changed release license/notice: ' + name)
