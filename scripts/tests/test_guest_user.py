"""Guest account contracts through both image construction paths."""
from pathlib import Path
import io
import json
import stat
import sys
import argparse
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

    def test_other_brand_account_paths_and_repair_preserve_existing_home_data(self):
        profile = guest_user.GuestUserProfile('atlas', 'atlas_guest', '.atlas')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'etc').mkdir()
            (root / 'etc/passwd').write_text('root:x:0:0:root:/root:/bin/sh\n')
            (root / 'etc/group').write_text('root:x:0:\n')
            (root / 'etc/shadow').write_text('root:!:0:0:99999:7:::\n')
            old_source = root / 'home/lingxi/lingxi/projects/app/index.html'
            old_source.parent.mkdir(parents=True)
            old_source.write_text('keep me')

            guest_user.apply_tree(root, profile)
            self.assertIn('atlas_guest:x:1000:1000:atlas:/home/atlas_guest:/bin/sh', (root / 'etc/passwd').read_text())
            self.assertIn('atlas_guest:x:1000:', (root / 'etc/group').read_text())
            self.assertIn('atlas_guest:!:', (root / 'etc/shadow').read_text())
            self.assertEqual((root / 'etc/doas.conf').read_text(), 'permit nopass atlas_guest as root\n')
            self.assertEqual((root / 'home/lingxi/lingxi/projects/app/index.html').read_text(), 'keep me')
            self.assertTrue((root / 'home/atlas_guest/atlas/projects').is_dir())

    def test_custom_brand_rejects_preprovisioned_foreign_uid_and_gid_without_rewriting_source(self):
        profile = guest_user.GuestUserProfile('atlas', 'atlas_guest', '.atlas')
        files = {
            'etc/passwd': 'root:x:0:0:root:/root:/bin/sh\nlingxi:x:1000:1000:LingXi:/home/lingxi:/bin/sh\n',
            'etc/group': 'root:x:0:\nlingxi:x:1000:\n',
            'etc/shadow': 'root:!:0:0:99999:7:::\nlingxi:!:0:0:99999:7:::\n',
        }
        original = dict(files)

        def read(path):
            return files.get(path, '')

        def write(path, text, _mode):
            files[path] = text

        with self.assertRaisesRegex(ValueError, 'UID 1000 belongs to .lingxi.'):
            guest_user.apply_account_profile(read, write, lambda _path, _mode: None, profile)

        self.assertEqual(files, original)

    def test_archive_rejects_preprovisioned_foreign_brand_account(self):
        profile = guest_user.GuestUserProfile('atlas', 'atlas_guest', '.atlas')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'lingxi-rootfs.tar'
            with tarfile.open(source, 'w') as tar:
                for path, contents in {
                    'etc/passwd': b'root:x:0:0:root:/root:/bin/sh\nlingxi:x:1000:1000:LingXi:/home/lingxi:/bin/sh\n',
                    'etc/group': b'root:x:0:\nlingxi:x:1000:\n',
                    'etc/shadow': b'root:!:0:0:99999:7:::\nlingxi:!:0:0:99999:7:::\n',
                }.items():
                    item = tarfile.TarInfo(path)
                    item.size = len(contents)
                    tar.addfile(item, io.BytesIO(contents))
            with mock.patch.object(guest_user, 'PINS', root / 'pins.json'):
                (root / 'pins.json').write_text(json.dumps({'guest_user_packages': {'aarch64': []}}))
                with self.assertRaisesRegex(ValueError, 'UID 1000 belongs to .lingxi.'):
                    guest_user.apply_archive(
                        source,
                        root / 'atlas.tar.gz',
                        'aarch64',
                        root / 'cache',
                        profile,
                    )
            self.assertFalse((root / 'atlas.tar.gz').exists())

    def test_other_brand_archive_uses_matching_account_and_tar_ownership(self):
        profile = guest_user.GuestUserProfile('atlas', 'atlas_guest', '.atlas')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'base.tar'
            with tarfile.open(source, 'w') as tar:
                data = b'P:busybox\nV:1\n\n'
                item = tarfile.TarInfo('lib/apk/db/installed'); item.size = len(data)
                tar.addfile(item, io.BytesIO(data))
            with mock.patch.object(guest_user, 'PINS', root / 'pins.json'):
                (root / 'pins.json').write_text(json.dumps({'guest_user_packages': {'aarch64': []}}))
                guest_user.apply_archive(source, root / 'atlas.tar.gz', 'aarch64', root / 'cache', profile)
            with tarfile.open(root / 'atlas.tar.gz') as tar:
                passwd = tar.extractfile('etc/passwd').read().decode()
                self.assertIn('atlas_guest:x:1000:1000:atlas:/home/atlas_guest:/bin/sh', passwd)
                for path in ('home/atlas_guest', 'home/atlas_guest/atlas', 'home/atlas_guest/atlas/projects'):
                    member = tar.getmember(path)
                    self.assertEqual((member.uid, member.gid, member.uname, member.gname),
                                     (1000, 1000, 'atlas_guest', 'atlas_guest'))

    def test_rootfs_tree_packager_assigns_owner_from_the_selected_brand_profile(self):
        profile = guest_user.GuestUserProfile('atlas', 'atlas_guest', '.atlas')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            user_home = root / 'home/atlas_guest/atlas/projects'
            user_home.mkdir(parents=True)
            (user_home / 'source.txt').write_text('user data')
            (root / 'etc').mkdir()
            (root / 'etc/passwd').write_text('atlas_guest:x:1000:1000:atlas:/home/atlas_guest:/bin/sh\n')
            output = root.parent / (root.name + '-rootfs.tar')
            try:
                rootfs_tool.set_guest_profile(profile)
                with mock.patch.object(rootfs_tool, 'validate_rootfs_tree'):
                    rootfs_tool.build_archive(argparse.Namespace(root=root, output=output, source_date_epoch=0))
                with tarfile.open(output) as tar:
                    for path in ('home/atlas_guest', 'home/atlas_guest/atlas',
                                 'home/atlas_guest/atlas/projects',
                                 'home/atlas_guest/atlas/projects/source.txt'):
                        member = tar.getmember(path)
                        self.assertEqual((member.uid, member.gid, member.uname, member.gname),
                                         (1000, 1000, 'atlas_guest', 'atlas_guest'))
                    self.assertEqual(tar.getmember('etc').uid, 0)
            finally:
                rootfs_tool.set_guest_profile(guest_user.DEFAULT_PROFILE)
                output.unlink(missing_ok=True)

    def test_privileged_exception_rejects_unpinned_binary_and_other_modes(self):
        self.assertFalse(rootfs_tool.permitted_doas('/usr/bin/doas', 0o4755, b'changed binary'))
        self.assertFalse(rootfs_tool.permitted_doas('/usr/bin/other', 0o4755, b'changed binary'))
        self.assertFalse(rootfs_tool.permitted_doas('/usr/bin/doas', 0o6755, b'changed binary'))
