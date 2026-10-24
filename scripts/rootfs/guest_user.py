#!/usr/bin/env python3
"""Deterministic mobile guest account layer, shared by Android and iOS.

Never execute guest binaries on the build host. The base archive is verified
before conversion; added APK payloads are verified against SDK pins.
"""
import argparse
import copy
import gzip
import hashlib
import io
import json
import pathlib
import tarfile
import urllib.request

USER = 'lingxi'
HOME = '/home/lingxi'
WORKSPACE = HOME + '/lingxi'
UID = 1000
PINS = pathlib.Path(__file__).resolve().parents[2] / 'docs/toolchains/runtime-pins.json'


def account_lines(text, name, value):
    return '\n'.join([line for line in text.splitlines() if line.split(':')[0] != name] + [value]) + '\n'


def profile(read, write, mkdir):
    write('etc/passwd', account_lines(read('etc/passwd'), USER, 'lingxi:x:1000:1000:LingXi:/home/lingxi:/bin/sh'), 0o644)
    write('etc/group', account_lines(read('etc/group'), USER, 'lingxi:x:1000:'), 0o644)
    write('etc/shadow', account_lines(read('etc/shadow'), USER, 'lingxi:!:0:0:99999:7:::'), 0o640)
    write('etc/doas.conf', 'permit nopass lingxi as root\n', 0o600)
    for path, mode in [('home', 0o755), ('home/lingxi', 0o755), ('home/lingxi/lingxi', 0o755), ('root', 0o700)]:
        mkdir(path, mode)


def apply_tree(root):
    root = pathlib.Path(root)
    def read(path):
        p = root / path
        return p.read_text() if p.exists() else ''
    def write(path, data, mode):
        p = root / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(data); p.chmod(mode)
    def mkdir(path, mode):
        p = root / path
        p.mkdir(parents=True, exist_ok=True); p.chmod(mode)
    profile(read, write, mkdir)
    doas = root / 'usr/bin/doas'
    if doas.exists():
        pins = json.loads(PINS.read_text())
        if doas.is_symlink() or not doas.is_file() or hashlib.sha256(doas.read_bytes()).hexdigest() not in pins['guest_user_doas_sha256'].values():
            raise ValueError('unapproved privileged guest doas binary')
        doas.chmod(0o4755)
        if doas.stat().st_mode & 0o7777 != 0o4755:
            raise ValueError("guest doas SUID permission could not be retained by the build filesystem")
    # APK logs contain wall-clock timestamps; build receipts retain installation
    # output separately. Do not put a non-reproducible build log in the image.
    log = root / 'var/log/apk.log'
    if log.is_file() and not log.is_symlink(): log.unlink()


def apk_payload(record, cache):
    cache.mkdir(parents=True, exist_ok=True)
    target = cache / pathlib.PurePosixPath(record['url']).name
    if not target.exists() or hashlib.sha256(target.read_bytes()).hexdigest() != record['sha256']:
        data = urllib.request.urlopen(record['url'], timeout=60).read()
        if hashlib.sha256(data).hexdigest() != record['sha256']:
            raise ValueError('guest account APK digest mismatch: ' + record['name'])
        target.write_bytes(data)
    # APK v2 is concatenated gzip/tar streams. tarfile's ignore_zeros scans
    # past the signature/control stream to the data stream.
    return tarfile.open(fileobj=io.BytesIO(target.read_bytes()), mode='r:gz', ignore_zeros=True)


def apply_archive(source, output, arch, cache):
    entries = {}
    def ingest(tar, package=False):
        metadata = ''
        for item in tar:
            name = item.name.removeprefix('./').rstrip('/')
            if not name or name == '.':
                if not package:
                    if not item.isdir():
                        raise ValueError('guest archive root must be a directory')
                    item = copy.copy(item); item.name = '.'
                    entries['.'] = item, None
                continue
            if name.startswith('/') or '..' in pathlib.PurePosixPath(name).parts:
                raise ValueError('unsafe guest archive path: ' + name)
            payload = tar.extractfile(item).read() if item.isfile() else None
            if package and name.startswith('.'):
                if name == '.PKGINFO': metadata = payload.decode()
                continue
            if item.isdev() or item.isfifo():
                raise ValueError('unsupported guest archive entry: ' + name)
            item = copy.copy(item); item.name = name
            entries[name] = (item, payload)
        return metadata
    with tarfile.open(source, 'r:*') as tar: ingest(tar)
    # fakefsify's fallback for an absent root stores permission bits without
    # S_IFDIR. Keep an explicit tar directory so guest chdir("/") sees a dir.
    if '.' not in entries:
        root = tarfile.TarInfo('.')
        root.type = tarfile.DIRTYPE; root.mode = 0o755
        entries['.'] = root, None
    pins = json.loads(PINS.read_text())
    for record in pins['guest_user_packages'][arch]:
        with apk_payload(record, pathlib.Path(cache) / arch) as tar:
            ingest(tar, package=True)
        # The APK database record is captured from the pinned offline APK
        # installation, including its package identity, file checksums and modes.
        # Keeping that exact record makes apk info/upgrade own these files too.
        installed = record['installed_record']
        if ('P:' + record['name']) not in installed.splitlines() or ('V:' + record['version']) not in installed.splitlines():
            raise ValueError('guest account installed-package record mismatch')
        database = '\n\n'.join(block for block in entries['lib/apk/db/installed'][1].decode().strip().split('\n\n') if ('P:' + record['name']) not in block.splitlines()) + '\n\n' + installed
        item = copy.copy(entries['lib/apk/db/installed'][0]); data = database.encode(); item.size = len(data)
        entries[item.name] = item, data
    def read(path): return entries[path][1].decode() if path in entries else ''
    def write(path, text, mode):
        data = text.encode(); item = tarfile.TarInfo(path); item.mode = mode; item.size = len(data)
        entries[path] = item, data
    def mkdir(path, mode):
        item = tarfile.TarInfo(path); item.type = tarfile.DIRTYPE; item.mode = mode
        entries[path] = item, None
    profile(read, write, mkdir)
    entries.pop('var/log/apk.log', None)
    with open(output, 'wb') as file, gzip.GzipFile(filename='', fileobj=file, mode='wb', mtime=0) as zipped, tarfile.open(fileobj=zipped, mode='w') as tar:
        for path in sorted(entries):
            item, data = entries[path]
            item.mtime = 0
            if path == 'home/lingxi' or path.startswith('home/lingxi/'):
                item.uid = item.gid = UID; item.uname = item.gname = USER
            tar.addfile(item, io.BytesIO(data) if data is not None else None)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--root')
    parser.add_argument('--archive'); parser.add_argument('--output')
    parser.add_argument('--arch'); parser.add_argument('--cache')
    args = parser.parse_args()
    if args.root: apply_tree(args.root)
    else: apply_archive(args.archive, args.output, args.arch, args.cache)
