#!/usr/bin/env python3
"""Guard the iSH patch cache and device archive's required policy hooks."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import subprocess
import unittest
import sqlite3
import struct
from unittest import mock

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS / 'lib'))
import ios_native_build

spec = importlib.util.spec_from_file_location('verify_ios_native', SCRIPTS / 'checks/verify-ios-native.py')
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


class IosNativeBuildTests(unittest.TestCase):
    def test_fakefs_root_verification_rejects_permission_only_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'data').mkdir()
            with sqlite3.connect(root/'meta.db') as connection:
                connection.executescript('create table paths(path blob primary key,inode integer); create table stats(inode integer primary key,stat blob);')
                connection.execute("insert into paths values(x'',1)")
                for mode in [0o40755, 0o755, 0o100755, 0o120777]:
                    with self.subTest(mode=oct(mode)):
                        connection.execute('insert or replace into stats values(1,?)', (struct.pack('<IIII',mode,0,0,0),))
                        connection.commit()
                        if mode == 0o40755:
                            self.assertEqual(ios_native_build.verify_fakefs_root(root), mode)
                        else:
                            with self.assertRaisesRegex(ValueError,'directory type'):
                                ios_native_build.verify_fakefs_root(root)

    def test_final_worker_exit_notifies_executor_process_identity(self):
        sdk = SCRIPTS.parent
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root/'src/ios/iSH/ISHKernel.m'
            source.parent.mkdir(parents=True)
            source.write_bytes((sdk/'native/ios/upstream/openminis/src/ios/iSH/ISHKernel.m').read_bytes())
            subprocess.run(['patch','-F','0','-p1','-d',str(root),'-i',str(sdk/'native/ios/patches/openminis-process-exit-leader.patch')],check=True)
            text = source.read_text()
            start = text.index('static void handle_process_exit(')
            end = text.index('    dispatch_async(', start)
            prelude = text[start:end].replace('static void handle_process_exit(', 'static int notification_pid(').replace('        return;', '        return -1;')
            probe = root/'exit-identity.c'
            probe.write_text('#include <assert.h>\n#include <stddef.h>\n#include <sys/types.h>\nstruct task; struct group { struct task *leader; }; struct task { int pid; struct task *parent; struct group *group; };\n' + prelude + '    (void)code; return pid;\n}\n' + r"""
int main(void) {
    struct group init_group, process_group, child_group;
    struct task init = {1, 0, &init_group}; init_group.leader = &init;
    struct task process = {3, &init, &process_group}; process_group.leader = &process;
    struct task worker = {25, &process, &process_group};
    assert(notification_pid(&worker, 15) == 3);
    assert(notification_pid(&process, 15) == 3);
    assert(notification_pid(&init, 0) == 1);
    struct task child = {30, &process, &child_group}; child_group.leader = &child;
    struct task child_worker = {31, &child, &child_group};
    assert(notification_pid(&child_worker, 0) == -1);
    assert(notification_pid(&child, 0) == -1);
    return 0;
}
""")
            binary = root/'exit-identity'
            subprocess.run(['cc','-std=c11','-Wall','-Wextra','-Werror',str(probe),'-o',str(binary)],check=True)
            subprocess.run([str(binary)],check=True)

    def test_prepared_cache_requires_all_policy_patches(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'prepared').write_text('identity')
            for relative, symbols in {**ios_native_build.PATCH_MARKERS, **ios_native_build.SOURCE_PATCH_MARKERS}.items():
                source = work / 'ish' / relative
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text('\n'.join(symbols))
            self.assertTrue(ios_native_build.prepared_work_matches(work, 'identity'))
            self.assertFalse(ios_native_build.prepared_work_matches(work, 'different'))
            (work / 'ish/fs/sock.c').write_text('unpatched source')
            self.assertFalse(ios_native_build.prepared_work_matches(work, 'identity'))

    def test_mount_path_guard_does_not_follow_home_or_accept_replaced_links(self):
        sdk = SCRIPTS.parent
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'mount-path-probe'
            subprocess.run(['cc', '-std=c11', '-Wall', '-Wextra', '-Werror',
                            '-I', str(sdk / 'native/ios'),
                            str(SCRIPTS / 'tests/ios_mount_path_guard_probe.c'),
                            '-o', str(binary)], check=True)
            subprocess.run([str(binary), str(root)], check=True)

    def test_device_artifact_rejects_missing_policy_symbol(self):
        symbols = '\n'.join([
            '00000000 T _lx_ish_sock_policy_hook_version',
            '00000001 T _lx_ish_guest_execution_resident_bytes',
        ])
        with mock.patch.object(verifier.sys, 'platform', 'darwin'), mock.patch.object(
            verifier.subprocess, 'check_output', return_value=symbols
        ):
            with self.assertRaisesRegex(SystemExit, 'lx_ish_guest_execution_context_active'):
                verifier.verify_device_policy_symbols(Path('/artifact'), {'iphoneos-arm64'})


if __name__ == '__main__':
    unittest.main()
