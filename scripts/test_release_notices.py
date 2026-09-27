#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import tempfile
import unittest
import zipfile
import sys
sys.dont_write_bytecode=True
from release_notices import FILES,stage,verify
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('package_sdk',Path(__file__).with_name('package-sdk.py'))
package=importlib.util.module_from_spec(spec);spec.loader.exec_module(package)
class NoticeTests(unittest.TestCase):
    def test_original_notices_survive_binary_and_swift_zip(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);licenses=root/'licenses';stage(ROOT,licenses);verify(ROOT,licenses)
            binary=root/'FFI.xcframework';binary.mkdir();(binary/'library').write_bytes(b'native')
            for name in ['ffi.zip','swift.zip']:
                target=root/name;package.archive(binary,target,'payload',[licenses])
                with zipfile.ZipFile(target) as archive:
                    for license_name,source in FILES.items():self.assertEqual(archive.read('licenses/'+license_name),(ROOT/source).read_bytes())
    def test_missing_notice_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            target=Path(temporary);stage(ROOT,target);(target/'platform-pty-NOTICE').unlink()
            with self.assertRaisesRegex(ValueError,'notice'):verify(ROOT,target)
    def test_changed_license_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            target=Path(temporary);stage(ROOT,target);(target/'SDK-LICENSE').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError,'license'):verify(ROOT,target)
if __name__=='__main__':unittest.main()
