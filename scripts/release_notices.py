"""Original SDK and imported PTY notices shipped with every Rust FFI artifact."""
from pathlib import Path
import hashlib
import shutil
FILES = {'SDK-LICENSE': 'LICENSE', 'SDK-LICENSE-APACHE': 'LICENSE-APACHE', 'platform-pty-NOTICE': 'crates/platform-pty/NOTICE',
         'platform-pty-LICENSE-APACHE': 'crates/platform-pty/LICENSE-APACHE',
         'COMPONENT-NOTICE.md': 'docs/mobile-linux/LICENSES/NOTICE.md'}

def stage(root, destination):
    destination.mkdir(parents=True, exist_ok=True)
    for name, source in FILES.items():
        shutil.copy2(root / source, destination / name)

def verify(root, directory):
    for name, source in FILES.items():
        if not (directory/name).is_file() or (directory/name).read_bytes() != (root/source).read_bytes():
            raise ValueError('missing or changed release license/notice: ' + name)
