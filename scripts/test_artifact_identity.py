#!/usr/bin/env python3
import json,pathlib,tempfile,unittest
import sys
sys.dont_write_bytecode = True
from sdk_artifact_identity import file_hashes,validate_artifacts,validate_ios_native,validate_swift_binding
class ArtifactIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=pathlib.Path(self.temp.name)
        (self.root/"library.so").write_bytes(b"native");self.manifest={"source_revision":"a"*40,"source_dirty":False,"platform":"android","namespace":"mobile_linux_runtime","files":file_hashes(self.root,"ffi-build.json")};self.write()
    def write(self):(self.root/"ffi-build.json").write_text(json.dumps(self.manifest))
    def verify(self):return validate_artifacts(self.root,"ffi-build.json","a"*40,expected={"platform":"android","namespace":"mobile_linux_runtime"})
    def test_current(self):self.verify()
    def test_stale(self):
        self.manifest["source_revision"]="b"*40;self.write()
        with self.assertRaisesRegex(ValueError,"stale"):self.verify()
    def test_tamper(self):
        (self.root/"library.so").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError,"bytes changed"):self.verify()
    def test_missing_metadata(self):
        (self.root/"ffi-build.json").unlink()
        with self.assertRaises(FileNotFoundError):self.verify()
    def test_extra_file(self):
        (self.root/"unexpected.so").write_bytes(b"x")
        with self.assertRaisesRegex(ValueError,"file set"):self.verify()
    def test_dirty_cannot_release(self):
        self.manifest["source_dirty"]=True;self.write()
        with self.assertRaisesRegex(ValueError,"not releasable"):self.verify()
class IosArtifactIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=pathlib.Path(self.temp.name);self.framework=self.root/"SDK.xcframework";self.framework.mkdir();(self.framework/"binary").write_bytes(b"ios")
        for name in ["licenses", "source-provenance"]:
            (self.root/name).mkdir();(self.root/name/"notice").write_text("original")
        self.manifest={"source_revision":"a"*40,"source_dirty":False,"kind":"native-support","contains_rust":False,"slices":["iphoneos-arm64","iphonesimulator-arm64","iphonesimulator-x86_64"],"source_file_sha256":{"input":"unused-in-fixture"},"artifacts":file_hashes(self.root,"native-support-manifest.json")};self.write()
    def write(self):(self.root/"native-support-manifest.json").write_text(json.dumps(self.manifest))
    def verify(self):validate_ios_native(self.root,self.framework,"a"*40)
    def test_current_native(self):self.verify()
    def test_stale_native(self):
        self.manifest["source_revision"]="b"*40;self.write()
        with self.assertRaisesRegex(ValueError,"stale"):self.verify()
    def test_modified_native(self):
        (self.framework/"binary").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError,"bytes changed"):self.verify()
    def test_extra_framework_file(self):
        (self.framework/"injected").write_bytes(b"x")
        with self.assertRaisesRegex(ValueError,"file set"):self.verify()
    def test_extra_license_file(self):
        (self.root/"licenses"/"injected").write_bytes(b"x")
        with self.assertRaisesRegex(ValueError,"file set"):self.verify()
    def test_tampered_license(self):
        (self.root/"licenses"/"notice").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError,"bytes changed"):self.verify()
    def test_mixed_swift_binding(self):
        binding=self.root/"other.swift";binding.write_bytes(b"different-generation")
        with self.assertRaisesRegex(ValueError,"generation"):validate_swift_binding(binding,{"files":{"swift/MobileLinuxRuntimeBindings.swift":"0"*64}})
if __name__=="__main__":unittest.main()
