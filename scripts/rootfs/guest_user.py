#!/usr/bin/env python3
"""Deterministic mobile guest account layer, shared by Android and iOS.

Never execute guest binaries on the build host. The base archive is verified
before conversion; added APK payloads are verified against SDK pins.
"""
import argparse
import copy
from dataclasses import dataclass
import gzip
import hashlib
import io
import json
import pathlib
import re
import tarfile
import urllib.request

UID = 1000
PINS = pathlib.Path(__file__).resolve().parents[2] / 'docs/toolchains/runtime-pins.json'


@dataclass(frozen=True)
class GuestUserProfile:
    """The brand-scoped account identity embedded in a mobile rootfs."""
    brand_id: str
    linux_user: str
    config_dir_name: str

    def validate(self):
        if not re.fullmatch(r'[a-z0-9][a-z0-9._-]{0,62}', self.brand_id):
            raise ValueError('brand id must be one lowercase path component')
        if not re.fullmatch(r'[a-z_][a-z0-9_-]{0,31}', self.linux_user) or self.linux_user == 'root':
            raise ValueError('Linux user must be a safe non-root account name')
        if (not re.fullmatch(r'\.[a-z0-9][a-z0-9._-]{0,62}', self.config_dir_name)
                or self.config_dir_name in {'.', '..'}):
            raise ValueError('config directory name must be one hidden path component')
        return self

    @property
    def guest_home(self):
        return pathlib.PurePosixPath('/home') / self.linux_user

    @property
    def workspace_root(self):
        return self.guest_home / self.brand_id

    @property
    def projects_root(self):
        return self.workspace_root / 'projects'


DEFAULT_PROFILE = GuestUserProfile('lingxi', 'lingxi', '.lingxi')


def profile_from_values(brand_id=None, linux_user=None, config_dir_name=None):
    return GuestUserProfile(
        DEFAULT_PROFILE.brand_id if brand_id is None else brand_id,
        DEFAULT_PROFILE.linux_user if linux_user is None else linux_user,
        DEFAULT_PROFILE.config_dir_name if config_dir_name is None else config_dir_name,
    ).validate()


def account_lines(text, name, value):
    return '\n'.join([line for line in text.splitlines() if line.split(':')[0] != name] + [value]) + '\n'


def _identity_entries(text, field_index):
    return [
        fields
        for line in text.splitlines()
        if len(fields := line.split(':')) > field_index and fields[field_index] == str(UID)
    ]


def _validate_runtime_identity_source(read, profile):
    """Fail closed if a base image already assigns UID/GID 1000 to another identity."""
    passwd = read('etc/passwd')
    groups = read('etc/group')
    uid_entries = _identity_entries(passwd, 2)
    gid_entries = _identity_entries(groups, 2)
    for kind, entries in (('UID', uid_entries), ('GID', gid_entries)):
        if len(entries) > 1:
            raise ValueError(f'base rootfs has multiple {kind} 1000 entries; refusing brand identity provisioning')
        if entries and entries[0][0] != profile.linux_user:
            raise ValueError(
                f'base rootfs {kind} 1000 belongs to {entries[0][0]!r}, not selected brand account '
                f'{profile.linux_user!r}; use a matching or unbranded rootfs'
            )


def apply_account_profile(read, write, mkdir, profile):
    profile.validate()
    _validate_runtime_identity_source(read, profile)
    user = profile.linux_user
    home = profile.guest_home.as_posix().lstrip('/')
    workspace = profile.workspace_root.as_posix().lstrip('/')
    projects = profile.projects_root.as_posix().lstrip('/')
    gecos = 'LingXi' if profile == DEFAULT_PROFILE else profile.brand_id
    passwd = f'{user}:x:{UID}:{UID}:{gecos}:{profile.guest_home}:/bin/sh'
    write('etc/passwd', account_lines(read('etc/passwd'), user, passwd), 0o644)
    write('etc/group', account_lines(read('etc/group'), user, f'{user}:x:{UID}:'), 0o644)
    write('etc/shadow', account_lines(read('etc/shadow'), user, f'{user}:!:0:0:99999:7:::'), 0o640)
    write('etc/doas.conf', f'permit nopass {user} as root\n', 0o600)
    for path, mode in [('home', 0o755), (home, 0o755), (workspace, 0o755), (projects, 0o755), ('root', 0o700)]:
        mkdir(path, mode)


def apply_tree(root, profile=DEFAULT_PROFILE):
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
    apply_account_profile(read, write, mkdir, profile)
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


def apply_archive(source, output, arch, cache, profile=DEFAULT_PROFILE):
    profile.validate()
    entries = {}
    def ingest(tar, package=False):
        metadata = ''
        for item in tar:
            name = item.name.removeprefix('./').rstrip('/')
            if not name or name == '.': continue
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
    apply_account_profile(read, write, mkdir, profile)
    entries.pop('var/log/apk.log', None)
    with open(output, 'wb') as file, gzip.GzipFile(filename='', fileobj=file, mode='wb', mtime=0) as zipped, tarfile.open(fileobj=zipped, mode='w') as tar:
        for path in sorted(entries):
            item, data = entries[path]
            item.mtime = 0
            home = profile.guest_home.as_posix().lstrip('/')
            if path == home or path.startswith(home + '/'):
                item.uid = item.gid = UID; item.uname = item.gname = profile.linux_user
            tar.addfile(item, io.BytesIO(data) if data is not None else None)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--root')
    parser.add_argument('--archive'); parser.add_argument('--output')
    parser.add_argument('--arch'); parser.add_argument('--cache')
    parser.add_argument('--brand-id')
    parser.add_argument('--linux-user')
    parser.add_argument('--config-dir-name')
    args = parser.parse_args()
    profile = profile_from_values(args.brand_id, args.linux_user, args.config_dir_name)
    if args.root: apply_tree(args.root, profile)
    else: apply_archive(args.archive, args.output, args.arch, args.cache, profile)
