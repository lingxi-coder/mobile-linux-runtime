#!/usr/bin/env python3
"""Install an explicitly pinned release Maven ZIP without trusting ZIP metadata."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import tempfile
import urllib.parse
import urllib.request
import zipfile

MAX_ARCHIVE = 2 * 1024**3
MAX_EXPANDED = 4 * 1024**3
MAX_FILES = 100_000


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def verify(root, version, revision):
    if (root / 'sdk-artifacts.json').stat().st_size > 16 * 1024**2:
        raise ValueError('Maven manifest size limit exceeded')
    manifest = json.loads((root / 'sdk-artifacts.json').read_text())
    if (manifest.get('version') != version or manifest.get('source_revision') != revision
            or manifest.get('source_dirty') is not False or manifest.get('validation_only') is not False
            or manifest.get('native_support_only') is not False):
        raise ValueError('Maven release identity or cleanliness mismatch')
    expected = manifest.get('files')
    if not isinstance(expected, dict) or not expected:
        raise ValueError('missing exact Maven file inventory')
    actual = {}
    for path in root.rglob('*'):
        if path.is_symlink():
            raise ValueError('Maven symlinks are forbidden')
        if path.is_file() and path not in (root / 'sdk-artifacts.json', root / '.bundle-sha256'):
            actual[path.relative_to(root).as_posix()] = digest(path)
    if actual != expected:
        raise ValueError('Maven file inventory or hash mismatch')
    return manifest


class HTTPSOnly(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        require_https(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def require_https(url):
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
        raise ValueError('an explicit HTTPS URL without credentials or fragment is required')


def extract(archive, destination):
    with zipfile.ZipFile(archive) as bundle:
        infos = bundle.infolist()
        if len(infos) > MAX_FILES or sum(i.file_size for i in infos) > MAX_EXPANDED:
            raise ValueError('ZIP resource limit exceeded')
        seen = set()
        for info in infos:
            path = PurePosixPath(info.filename)
            mode = info.external_attr >> 16
            if (info.filename == '.bundle-sha256' or not info.filename or '\\' in info.filename or '\x00' in info.orig_filename
                    or path.is_absolute() or any(part in ('', '.', '..') for part in info.filename.rstrip('/').split('/'))
                    or path.parts[0].endswith(':') or str(path).casefold() in seen
                    or (stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR))
                    or info.flag_bits & 1):
                raise ValueError('unsafe or duplicate ZIP path/type')
            seen.add(str(path).casefold())
            target = destination.joinpath(*path.parts)
            if info.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            with bundle.open(info) as source, target.open('xb') as output:
                remaining = info.file_size
                while chunk := source.read(min(1024 * 1024, remaining + 1)):
                    remaining -= len(chunk)
                    if remaining < 0:
                        raise ValueError('ZIP size mismatch')
                    output.write(chunk)
                if remaining:
                    raise ValueError('truncated ZIP member')


def install(*, archive=None, url=None, sha256, version, revision, cache, output):
    if not re.fullmatch(r'[0-9a-f]{64}', sha256) or not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError('full lowercase SHA256 and source revision required')
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', version):
        raise ValueError('invalid explicit version')
    if bool(archive) == bool(url):
        raise ValueError('specify exactly one archive or HTTPS URL')
    if Path(cache).is_symlink() or Path(output).is_symlink():
        raise ValueError('cache and destination must not be symlinks')
    if url:
        require_https(url)
    cache = Path(cache).resolve(); output = Path(output).resolve()
    cache.mkdir(parents=True, exist_ok=True)
    cached = cache / (sha256 + '.zip')
    if cached.exists():
        if cached.is_symlink() or not cached.is_file() or cached.stat().st_size > MAX_ARCHIVE or digest(cached) != sha256:
            raise ValueError('immutable archive cache mismatch')
    else:
        with tempfile.NamedTemporaryFile(dir=cache, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                if url:
                    require_https(url)
                    source = urllib.request.build_opener(HTTPSOnly()).open(url, timeout=60)
                else:
                    source = Path(archive).open('rb')
                with source:
                    size = 0
                    while chunk := source.read(1024 * 1024):
                        size += len(chunk)
                        if size > MAX_ARCHIVE:
                            raise ValueError('archive size limit exceeded')
                        stream.write(chunk)
                stream.flush()
                if digest(temporary) != sha256:
                    raise ValueError('archive SHA256 mismatch')
                os.link(temporary, cached)
            finally:
                temporary.unlink(missing_ok=True)
    if output.exists():
        verify(output, version, revision)
        # Existing output must also belong to this exact archive, not merely a same-ID rebuild.
        if (output / '.bundle-sha256').read_text().strip() != sha256:
            raise ValueError('immutable installation archive mismatch')
        return output
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output.parent, prefix='.maven-stage-') as stage:
        staged = Path(stage) / 'repository'; staged.mkdir()
        extract(cached, staged)
        verify(staged, version, revision)
        # Keep identity marker outside the manifest inventory verification namespace.
        (staged / '.bundle-sha256').write_text(sha256 + '\n')
        os.rename(staged, output)
    return output


def main():
    p = argparse.ArgumentParser(description=__doc__)
    source = p.add_mutually_exclusive_group(required=True)
    source.add_argument('--archive', type=Path); source.add_argument('--url')
    p.add_argument('--sha256', required=True); p.add_argument('--version', required=True)
    p.add_argument('--source-revision', required=True)
    p.add_argument('--cache-dir', type=Path, required=True); p.add_argument('--output-dir', type=Path, required=True)
    a = p.parse_args()
    print(install(archive=a.archive, url=a.url, sha256=a.sha256, version=a.version,
                  revision=a.source_revision, cache=a.cache_dir, output=a.output_dir))


if __name__ == '__main__':
    main()
