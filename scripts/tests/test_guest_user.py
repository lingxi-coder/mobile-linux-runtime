"""Guest account contracts through both image construction paths."""
from pathlib import Path
import io
import json
import stat
import sys
import tarfile
import tempfile
import unittest
from unittest import mock
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'rootfs'))
import guest_user
import rootfs_tool


class GuestUserTests(unittest.TestCase):
    def test_tree_profile_preserves_root_and_locks_password_login(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'etc').mkdir()
            (root / 'etc/passwd').write_text('root:x:0:0:root:/root:/bin/sh\n')
            guest_user.apply_tree(root)
            first = (root / 'etc/passwd').read_bytes()
            guest_user.apply_tree(root)
            self.assertEqual(first, (root / 'etc/passwd').read_bytes())
            self.assertIn(b'root:x:0:0:root:/root:/bin/sh\n', first)
            self.assertIn(b'lingxi:x:1000:1000:LingXi:/home/lingxi:/bin/sh\n', first)
            self.assertTrue((root / 'home/lingxi/lingxi').is_dir())
            self.assertIn('lingxi:!:', (root / 'etc/shadow').read_text())
            self.assertEqual(stat.S_IMODE((root / 'etc/doas.conf').stat().st_mode), 0o600)
            self.assertEqual((root / 'etc/doas.conf').read_text(), 'permit nopass lingxi as root\n')

    def test_archive_records_guest_ownership_and_reproducible_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'base.tar'
            with tarfile.open(source, 'w') as tar:
                data = b'P:busybox\nV:1\n\n'
                item = tarfile.TarInfo('lib/apk/db/installed'); item.size = len(data)
                tar.addfile(item, io.BytesIO(data))
            # This isolates account metadata from the separately hash-checked APK loader.
            with mock.patch.object(guest_user, 'PINS', root / 'pins.json'):
                (root / 'pins.json').write_text(json.dumps({'guest_user_packages': {'aarch64': []}}))
                for output in ['one.tar.gz', 'two.tar.gz']:
                    guest_user.apply_archive(source, root / output, 'aarch64', root / 'cache')
            self.assertEqual((root / 'one.tar.gz').read_bytes(), (root / 'two.tar.gz').read_bytes())
            with tarfile.open(root / 'one.tar.gz') as tar:
                home = tar.getmember('home/lingxi')
                workspace = tar.getmember('home/lingxi/lingxi')
                self.assertEqual((home.uid, home.gid, workspace.uid, workspace.gid), (1000, 1000, 1000, 1000))
                config = tar.getmember('etc/doas.conf')
                self.assertEqual((config.uid, config.gid, config.mode), (0, 0, 0o600))
                self.assertEqual(tar.getmember('root').mode, 0o700)

    def test_privileged_exception_rejects_unpinned_binary_and_other_modes(self):
        self.assertFalse(rootfs_tool.permitted_doas('/usr/bin/doas', 0o4755, b'changed binary'))
        self.assertFalse(rootfs_tool.permitted_doas('/usr/bin/other', 0o4755, b'changed binary'))
        self.assertFalse(rootfs_tool.permitted_doas('/usr/bin/doas', 0o6755, b'changed binary'))
