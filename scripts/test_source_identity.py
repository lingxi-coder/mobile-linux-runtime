#!/usr/bin/env python3
"""Cargo checkout cleanliness exceptions tested against actual Git porcelain."""
from pathlib import Path
import subprocess
import tempfile
import unittest
import sys
sys.dont_write_bytecode=True
from sdk_artifact_identity import source_identity
class SourceIdentityTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name)
        self.git('init','-q');(self.root/'source').write_text('original');self.commit()
    def git(self,*args):
        return subprocess.check_output(['git','-C',str(self.root),'-c','user.name=SDK test','-c','user.email=sdk-test@example.invalid',*args],stderr=subprocess.STDOUT)
    def commit(self):self.git('add','.');self.git('commit','-qm','fixture')
    def dirty(self):return source_identity(self.root)[1]
    def test_clean_checkout(self):self.assertFalse(self.dirty())
    def test_only_empty_untracked_root_marker(self):
        (self.root/'.cargo-ok').touch();self.assertFalse(self.dirty())
        (self.root/'source').write_text('modified');self.assertTrue(self.dirty())
    def test_nonempty_marker(self):
        (self.root/'.cargo-ok').write_bytes(b'x');self.assertTrue(self.dirty())
    def test_symlink_marker(self):
        (self.root/'empty').touch();self.commit();(self.root/'.cargo-ok').symlink_to('empty');self.assertTrue(self.dirty())
    def test_nested_marker(self):
        (self.root/'nested').mkdir();(self.root/'nested/.cargo-ok').touch();self.assertTrue(self.dirty())
    def test_modified_tracked_marker(self):
        (self.root/'.cargo-ok').write_bytes(b'x');self.commit();(self.root/'.cargo-ok').write_bytes(b'');self.assertTrue(self.dirty())
    def test_staged_marker(self):
        (self.root/'.cargo-ok').touch();self.git('add','.cargo-ok');self.assertTrue(self.dirty())
    def test_directory_marker(self):
        (self.root/'.cargo-ok').mkdir();(self.root/'.cargo-ok/nested').touch();self.assertTrue(self.dirty())
    def test_other_untracked_file(self):
        (self.root/'.cargo-ok').touch();(self.root/'arbitrary').touch();self.assertTrue(self.dirty())
if __name__=='__main__':unittest.main()
