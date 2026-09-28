#!/usr/bin/env python3
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import zipfile
import sys
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('installer', Path(__file__).resolve().parents[1] / 'release/install-maven-bundle.py')
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)

class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name); self.archive = self.root/'bundle.zip'
        self.manifest = dict(version='0.1.0', source_revision='a'*40, source_dirty=False,
                             validation_only=False, native_support_only=False,
                             files={'group/artifact.aar': hashlib.sha256(b'aar').hexdigest()})
    def pack(self, extra=None, payload=b'aar'):
        with zipfile.ZipFile(self.archive,'w') as z:
            z.writestr('sdk-artifacts.json',json.dumps(self.manifest)); z.writestr('group/artifact.aar',payload)
            if extra:z.writestr(extra,b'bad')
        self.sha = m.digest(self.archive)
    def install(self):
        return m.install(archive=self.archive,sha256=self.sha,version='0.1.0',revision='a'*40,
                         cache=self.root/'cache',output=self.root/'installed')
    def test_verified_atomic_and_idempotent(self):
        self.pack(); dest=self.install(); self.assertEqual((dest/'group/artifact.aar').read_bytes(),b'aar'); self.assertEqual(self.install(),dest)
    def test_hash_mismatch(self):
        self.pack();self.sha='0'*64
        with self.assertRaisesRegex(ValueError,'SHA256'):self.install()
        self.assertFalse((self.root/'installed').exists())
    def test_inventory_tamper(self):
        self.pack(payload=b'bad')
        with self.assertRaisesRegex(ValueError,'inventory'):self.install()
        self.assertFalse((self.root/'installed').exists())
    def test_stale_revision(self):
        self.manifest['source_revision']='b'*40;self.pack()
        with self.assertRaisesRegex(ValueError,'identity'):self.install()
    def test_traversal(self):
        self.pack('../escaped')
        with self.assertRaisesRegex(ValueError,'unsafe'):self.install()
        self.assertFalse((self.root/'escaped').exists())
    def test_symlink(self):
        self.pack()
        with zipfile.ZipFile(self.archive,'a') as z:
            info=zipfile.ZipInfo('link');info.external_attr=0o120777<<16;z.writestr(info,'/tmp')
        self.sha=m.digest(self.archive)
        with self.assertRaisesRegex(ValueError,'unsafe'):self.install()
    def test_dirty_release(self):
        self.manifest['source_dirty']=True;self.pack()
        with self.assertRaisesRegex(ValueError,'identity'):self.install()
    def test_extra_file(self):
        self.pack('unexpected')
        with self.assertRaisesRegex(ValueError,'inventory'):self.install()
    def test_insecure_url(self):
        with self.assertRaises(ValueError):m.require_https('http://example.com/release.zip')
    def test_modified_existing_install(self):
        self.pack();dest=self.install();(dest/'group/artifact.aar').write_bytes(b'bad')
        with self.assertRaisesRegex(ValueError,'inventory'):self.install()

if __name__=='__main__':unittest.main()
