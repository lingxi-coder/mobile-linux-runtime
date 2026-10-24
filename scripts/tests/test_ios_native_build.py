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
import re
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

    def patched_function(self, patch_name, signature):
        patch = (SCRIPTS.parent/'native/ios/patches'/patch_name).read_text()
        source = '\n'.join(line[1:] for line in patch.splitlines() if line.startswith((' ', '+')) and not line.startswith('+++'))
        start = source.index(signature)
        brace = source.index('{', start)
        depth = 1
        end = brace + 1
        while depth:
            depth += (source[end] == '{') - (source[end] == '}')
            end += 1
        return source[start:end]

    def test_truncate_syscalls_resolve_absolute_and_relative_paths_from_cwd(self):
        patch = (SCRIPTS.parent/'native/ios/patches/ish-truncate-cwd.patch').read_text()
        source = '\n'.join(line[1:] for line in patch.splitlines() if line.startswith((' ', '+')) and not line.startswith('+++'))
        functions = re.findall(r'dword_t sys_truncate64\([^)]*\)\s*\{.*?\n\}', source, re.S)
        self.assertEqual(len(functions), 2)
        code = r"""
#include <assert.h>
#include <stdint.h>
#include <stdbool.h>
#include <string.h>
#include <stddef.h>
#define MAX_PATH 4096
#define STRACE(...) ((void)0)
#define _EFAULT (-14)
struct fd { int unused; };
#define AT_PWD ((struct fd *)-2)
typedef unsigned long dword_t;
typedef uintptr_t addr_t;
typedef long long off_t_;
typedef uint64_t qword_t;
#define make_attr(kind, value) (value)
static const char *expected_path;
static off_t_ expected_size;
static int user_read_string(addr_t address, char *output, size_t size) {
    if (!address) return 1;
    assert(strlen((const char *)address) < size);
    strcpy(output, (const char *)address); return 0;
}
static int generic_setattrat(struct fd *at, const char *path, off_t_ size, bool follow) {
    assert(at == AT_PWD); assert(follow); assert(!strcmp(path, expected_path));
    assert(size == expected_size); return 37;
}
"""
        code += functions[0].replace('sys_truncate64(', 'probe_arm64(') + '\n'
        code += functions[1].replace('sys_truncate64(', 'probe_arm32(') + '\n'
        code += r"""
int main(void) {
    expected_path = "relative.dat"; expected_size = 19;
    assert(probe_arm64((addr_t)expected_path, expected_size) == 37);
    expected_path = "/tmp/absolute.dat"; expected_size = 9;
    assert(probe_arm64((addr_t)expected_path, expected_size) == 37);
    expected_path = "relative.dat"; expected_size = ((off_t_)7 << 32) | 9;
    assert(probe_arm32((addr_t)expected_path, 9, 7) == 37);
    expected_path = "/tmp/absolute.dat"; expected_size = 3;
    assert(probe_arm32((addr_t)expected_path, 3, 0) == 37);
    assert(probe_arm64(0, 0) == (dword_t)_EFAULT);
    assert(probe_arm32(0, 0, 0) == (dword_t)_EFAULT);
    return 0;
}
"""
        self.compile_native_probe(code)

    def compile_native_probe(self, code):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root/'probe.c'; source.write_text(code)
            binary = root/'probe'
            subprocess.run(['cc','-std=c11','-Wall','-Wextra','-Werror','-I',str(SCRIPTS.parent/'native/ios'),str(source),'-o',str(binary)],check=True)
            subprocess.run([str(binary)],check=True)

    def test_mount_installer_preserves_covered_files_directories_and_foreign_links(self):
        function = self.patched_function('ish-covered-path-preservation.patch', 'static int lx_mount_install_link(')
        code = r"""
#include <assert.h>
#include <stdlib.h>
#include "lx_mount_path_guard.h"
#define _EBUSY (-EBUSY)
#define _EINVAL (-EINVAL)
struct fakefs_bind_mount { bool materialized; char owned_link_target[PATH_MAX]; };
struct root_mount { int root_fd; };
static struct root_mount *g_fakefs_mount;
static const char *fix_path(const char *path) { return *path == '/' ? path + 1 : path; }
static int create_relative_symlink(int parent, const char *leaf, const char *target) { return symlinkat(target, parent, leaf); }
""" + function + r"""
int main(void) {
    char root[] = "/tmp/lx-covered-mount-XXXXXX"; assert(mkdtemp(root));
    struct root_mount fs = {open(root, O_RDONLY | O_DIRECTORY)}; assert(fs.root_fd >= 0); g_fakefs_mount = &fs;
    assert(mkdirat(fs.root_fd, "covered", 0700) == 0);
    int fd = openat(fs.root_fd, "covered/sentinel", O_CREAT | O_WRONLY, 0600); assert(fd >= 0); assert(write(fd, "keep", 4) == 4); close(fd);
    struct fakefs_bind_mount mount = {.materialized = true};
    assert(lx_mount_install_link(&mount, "/covered", "/unrelated", false) == 0); assert(!mount.materialized);
    char bytes[5] = {0}; fd = openat(fs.root_fd, "covered/sentinel", O_RDONLY); assert(fd >= 0); assert(read(fd, bytes, 4) == 4); close(fd); assert(!strcmp(bytes,"keep"));
    fd = openat(fs.root_fd,"file",O_CREAT|O_WRONLY,0600); assert(fd>=0); close(fd); mount.materialized=true;
    assert(lx_mount_install_link(&mount,"/file","/unrelated",false)==0); assert(!mount.materialized);
    assert(symlinkat("covered",fs.root_fd,"foreign")==0); mount.materialized=true;
    assert(lx_mount_install_link(&mount,"/foreign","/unrelated",false)==0); assert(!mount.materialized); assert(lx_mount_link_matches(fs.root_fd,"foreign","covered"));
    mount.materialized=true; assert(lx_mount_install_link(&mount,"/new","/unrelated",false)==0); assert(mount.materialized); assert(lx_mount_link_matches(fs.root_fd,"new",mount.owned_link_target));
    assert(unlinkat(fs.root_fd,"new",0)==0); assert(symlinkat("covered",fs.root_fd,"new")==0);
    assert(lx_mount_install_link(&mount,"/new","/replacement",true)==_EBUSY); assert(lx_mount_link_matches(fs.root_fd,"new","covered"));
    assert(unlinkat(fs.root_fd,"new",0)==0); assert(unlinkat(fs.root_fd,"foreign",0)==0); assert(unlinkat(fs.root_fd,"file",0)==0); assert(unlinkat(fs.root_fd,"covered/sentinel",0)==0); assert(unlinkat(fs.root_fd,"covered",AT_REMOVEDIR)==0); close(fs.root_fd); assert(rmdir(root)==0);
    const char *prefixes[] = {"/tmp"}; assert(lx_mount_longest_prefix("/tmp/a",prefixes,1)==0);
    return 0;
}
"""
        self.compile_native_probe(code)

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
