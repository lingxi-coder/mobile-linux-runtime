#!/usr/bin/env python3
"""Guard the iSH patch cache and device archive's required policy hooks."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS / 'lib'))
import ios_native_build

spec = importlib.util.spec_from_file_location('verify_ios_native', SCRIPTS / 'checks/verify-ios-native.py')
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


class IosNativeBuildTests(unittest.TestCase):
    def test_prepared_cache_requires_all_policy_patches(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            (work / 'prepared').write_text('identity')
            for relative, symbols in ios_native_build.PATCH_MARKERS.items():
                source = work / 'ish' / relative
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text('\n'.join(symbols))
            self.assertTrue(ios_native_build.prepared_work_matches(work, 'identity'))
            self.assertFalse(ios_native_build.prepared_work_matches(work, 'different'))
            (work / 'ish/fs/sock.c').write_text('unpatched source')
            self.assertFalse(ios_native_build.prepared_work_matches(work, 'identity'))

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
