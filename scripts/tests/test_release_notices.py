#!/usr/bin/env python3
import importlib.util
import hashlib
import shutil
from pathlib import Path
import tempfile
import unittest
import zipfile
import sys
sys.dont_write_bytecode=True
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "scripts/lib"))
from release_notices import FILES,registry_files,stage,verify
ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('package_sdk',Path(__file__).resolve().parents[1] / 'release/package-sdk.py')
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
    def test_both_sdk_licenses_are_hashed_build_inputs(self):
        self.assertEqual(FILES['SDK-LICENSE'], 'LICENSE')
        self.assertEqual(FILES['SDK-LICENSE-APACHE'], 'LICENSE-APACHE')
        spec = importlib.util.spec_from_file_location('build_ffi', Path(__file__).resolve().parents[1] / 'build/build-ffi.py')
        builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(builder)
        inputs = builder.source_inputs()
        for name in ['LICENSE', 'LICENSE-APACHE']:
            self.assertEqual(inputs[name], hashlib.sha256((ROOT / name).read_bytes()).hexdigest())

    def test_missing_notice_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            target=Path(temporary);stage(ROOT,target);(target/'platform-pty-NOTICE').unlink()
            with self.assertRaisesRegex(ValueError,'notice'):verify(ROOT,target)
    def test_changed_license_fails(self):
        for name in ['SDK-LICENSE', 'SDK-LICENSE-APACHE']:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                target=Path(temporary);stage(ROOT,target);(target/name).write_bytes(b'changed')
                with self.assertRaisesRegex(ValueError,'license'):verify(ROOT,target)

    def test_registry_notice_inventory_rejects_lock_and_text_drift(self):
        self.assertEqual(len(registry_files(ROOT)), 240)  # manifest plus 239 texts
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            shutil.copy2(ROOT/'Cargo.lock',root/'Cargo.lock')
            shutil.copytree(ROOT/'docs/licenses/rust',root/'docs/licenses/rust')
            with (root/'Cargo.lock').open('a') as lock:
                lock.write('\n[[package]]\nname = "unnoticed-crate"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "'+'0'*64+'"\n')
            with self.assertRaisesRegex(ValueError,'inventory differs'):registry_files(root)
            shutil.copy2(ROOT/'Cargo.lock',root/'Cargo.lock')
            (root/'docs/licenses/rust/files/uniffi-0.28.3-0-uniffi-0.28.3-MPL-2.0.txt').write_bytes(b'tampered')
            with self.assertRaisesRegex(ValueError,'committed digest'):registry_files(root)
if __name__=='__main__':unittest.main()
