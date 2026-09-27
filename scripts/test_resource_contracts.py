#!/usr/bin/env python3
import contextlib
import hashlib
import importlib.util
import io
import json
import re
from pathlib import Path
import sys
import tempfile
import tarfile
import unittest
from unittest import mock
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).parent / "mobile-linux"))
from source_contract import external_output, SOURCE_ROOT
from check_dependencies import validate


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).parent / "mobile-linux" / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

bundle_gate = load("bundle_gate", "verify-bundle.py")
pins_gate = load("pins_gate", "verify-toolchain-pins.py")
profile_gate = load("profile_gate", "verify-rootfs-profile.py")
evidence_gate = load("evidence_gate", "verify-evidence.py")
alias_gate = load("alias_gate", "normalize-interpreter-aliases.py")

class ResourceContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bundle = self.root / "bundle"
        self.bundle.mkdir()
        (self.bundle / "package.json").write_text('{"name":"sdk-test","private":true}')
        (self.bundle / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n")
        self.digest = hashlib.sha256((self.bundle / "pnpm-lock.yaml").read_bytes()).hexdigest()

    def verify_bundle(self, **kwargs):
        values = dict(bundle=self.bundle, digest=self.digest, output=self.root / "out", cache=self.root / "cache")
        values.update(kwargs)
        with contextlib.redirect_stdout(io.StringIO()):
            bundle_gate.verify(**values)

    def test_bounded_interpreter_normalization_preserves_bytes(self):
        root = self.root / "staging"
        for relative, target in (("bin/sh", "busybox"), ("usr/bin/python3", "python3.actual")):
            alias = root / relative
            alias.parent.mkdir(parents=True, exist_ok=True)
            binary = alias.parent / target
            binary.write_bytes(b"\x7fELF" + b"fixture" * 5)
            binary.chmod(0o755)
            alias.symlink_to(target)
        receipt = self.root / "receipt.json"
        with contextlib.redirect_stdout(io.StringIO()): alias_gate.normalize(root, receipt)
        data = json.loads(receipt.read_text())
        self.assertEqual(len(data["transformations"]), 2)
        for row in data["transformations"]:
            alias = root / row["path"].lstrip("/")
            target = root / row["target"].lstrip("/")
            self.assertFalse(alias.is_symlink())
            self.assertEqual(alias.stat().st_ino, target.stat().st_ino)
            self.assertEqual(row["before_sha256"], row["after_sha256"])

    def test_alias_escape_is_rejected(self):
        root = self.root / "staging"
        (root / "bin").mkdir(parents=True)
        (root / "bin/sh").symlink_to("../../bundle/package.json")
        with self.assertRaises(ValueError): alias_gate.normalize(root, self.root / "receipt.json")

    def test_archive_inventory_compares_hardlinks_symlinks_and_bytes(self):
        archive = self.root / "payload.tar"
        with tarfile.open(archive, "w") as tar:
            regular = tarfile.TarInfo("bin/tool")
            regular.size = 7
            tar.addfile(regular, io.BytesIO(b"payload"))
            hard = tarfile.TarInfo("bin/alias")
            hard.type, hard.linkname = tarfile.LNKTYPE, "bin/tool"
            tar.addfile(hard)
            sym = tarfile.TarInfo("bin/link")
            sym.type, sym.linkname = tarfile.SYMTYPE, "tool"
            tar.addfile(sym)
        expected = sorted([
            {"path":"/bin/"+name, "sha256":hashlib.sha256(payload).hexdigest(), "size_bytes":len(payload), "kind":kind}
            for name,payload,kind in (("tool",b"payload","regular-file"),("alias",b"payload","regular-file"),("link",b"tool","symlink"))
        ], key=lambda row:row["path"])
        evidence_gate.verify_archive_inventory(archive, expected)
        for mode in ("bytes", "missing", "extra", "kind"):
            altered = json.loads(json.dumps(expected))
            if mode == "bytes": altered[0]["sha256"] = "0" * 64
            if mode == "missing": altered.pop()
            if mode == "extra": altered.append(dict(altered[0], path="/extra"))
            if mode == "kind": altered[0]["kind"] = "symlink"
            with self.subTest(mode=mode), self.assertRaisesRegex(ValueError,"payload inventory"):
                evidence_gate.verify_archive_inventory(archive, altered)

    def test_base_profile_safe_guest_links_and_rejection(self):
        def fixture(mode):
            archive = self.root / (mode + ".tar")
            header = bytearray(64)
            header[:6] = b"\x7fELF\x02\x01"
            header[18:20] = (62 if mode == "wrong_arch" else 183).to_bytes(2,"little")
            with tarfile.open(archive,"w") as tar:
                for name,payload in (("bin/busybox",bytes(header)),("sbin/apk",bytes(header)),("etc/apk/repositories",b"fixture")):
                    entry=tarfile.TarInfo(name);entry.size=len(payload);tar.addfile(entry,io.BytesIO(payload))
                link=tarfile.TarInfo("bin/sh");link.type=tarfile.SYMTYPE
                link.linkname="../../escape" if mode == "escape" else "/bin/busybox"
                tar.addfile(link)
                if mode == "link_parent":
                    entry=tarfile.TarInfo("bin/sh/injected");tar.addfile(entry,io.BytesIO())
            return archive, hashlib.sha256(archive.read_bytes()).hexdigest()
        for mode in ("valid","wrong_arch","escape","link_parent"):
            archive,digest=fixture(mode)
            # Test-only fixture source identity; production CLI always uses the
            # SDK's committed Alpine pin and has no alternate-pins option.
            with mock.patch.dict(profile_gate.tool._PINS["alpine"]["minirootfs"]["aarch64"],sha256=digest):
                if mode == "valid":
                    with contextlib.redirect_stdout(io.StringIO()):
                        data=profile_gate.verify(archive,"aarch64","base",digest)
                    self.assertEqual(len(data["inventory"]),4)
                    with self.assertRaisesRegex(ValueError,"source pin"):
                        profile_gate.verify(archive,"aarch64","base","0"*64)
                else:
                    with self.assertRaises((ValueError,SystemExit)):
                        profile_gate.verify(archive,"aarch64","base",digest)

    def test_ci_supported_platform_minimums(self):
        workflow = (SOURCE_ROOT / ".github/workflows/ci.yml").read_text()
        android = re.findall(r"--platform\s+(\d+)", workflow)
        ios = re.findall(r'IPHONEOS_DEPLOYMENT_TARGET:\s*"([0-9.]+)"', workflow)
        self.assertTrue(android, "CI must actually build Android SDK")
        self.assertEqual(set(android), {"26"})
        self.assertEqual(ios, ["18.0", "18.0"], "Rust and native iOS jobs must explicitly use iOS18")
        self.assertIn("--android-api 26", workflow)
        self.assertNotIn("--kind sdk", workflow)
        self.assertIn("scripts/build-ffi.py --platform ios --release", workflow)
        for command in ("-p mobile-linux-android", "-p mobile-linux-ios", "scripts/test_artifact_identity.py", ":installer:testDebugUnitTest", "xcodebuild test", "MobileLinuxNativeTests"):
            self.assertIn(command, workflow, f"missing existing test ownership: {command}")

    def test_explicit_bundle_positive(self):
        self.verify_bundle()

    def test_changed_lock_is_rejected(self):
        (self.bundle / "pnpm-lock.yaml").write_text("tampered")
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            self.verify_bundle()

    def test_missing_input_is_rejected(self):
        (self.bundle / "package.json").unlink()
        with self.assertRaisesRegex(ValueError, "missing dependency"):
            self.verify_bundle()

    def test_bundle_symlink_is_rejected(self):
        (self.bundle / "escape").symlink_to(SOURCE_ROOT)
        with self.assertRaisesRegex(ValueError, "symlinks"):
            self.verify_bundle()

    def test_bundle_cannot_be_output_or_cache(self):
        for field in ("output", "cache"):
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, "overlaps"):
                self.verify_bundle(**{field:self.bundle / "nested"})

    def test_source_write_direct_and_symlink_rejected(self):
        alias = self.root / "alias"
        alias.symlink_to(SOURCE_ROOT, target_is_directory=True)
        for value in (SOURCE_ROOT / "generated", alias / "generated"):
            with self.assertRaisesRegex(ValueError, "overlaps"):
                external_output(value)

    def test_output_cache_overlap_rejected(self):
        with self.assertRaisesRegex(ValueError, "must not overlap"):
            self.verify_bundle(cache=self.root / "out" / "cache")

    def test_complete_sdk_pins_positive(self):
        with contextlib.redirect_stdout(io.StringIO()):
            pins_gate.verify()

    def test_pin_tamper_and_product_profile_rejected(self):
        original = json.loads(pins_gate.DEFAULT.read_text())
        for mode in ("hash", "profile", "closure", "license", "builder"):
            pins = json.loads(json.dumps(original))
            if mode == "hash": pins["node_source"]["sha256"] = "broken"
            if mode == "profile": pins["local_app_runtime"] = {}
            if mode == "closure": pins["apk_artifacts"]["arm64-v8a"]["artifacts"] = []
            if mode == "license": pins["node_source"]["license"] = ""
            if mode == "builder": pins["node_source"]["builder_images"]["aarch64"] = "alpine:latest"
            path = self.root / "pins.json"
            path.write_text(json.dumps(pins))
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                pins_gate.verify(path)

    def test_missing_real_apk_rejected(self):
        with self.assertRaisesRegex(ValueError, "missing or corrupt APK"):
            pins_gate.verify(apk_dir=self.root)

    def test_sdk_dependency_positive_and_reverse_negative(self):
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers=["crates/example"]\n')
        crate = self.root / "crates/example"
        crate.mkdir(parents=True)
        path = crate / "Cargo.toml"
        base = '[package]\nname="example"\nversion="0.1.0"\n'
        path.write_text(base)
        with contextlib.redirect_stdout(io.StringIO()): validate(self.root)
        for dep in ('harness-runtime="0.1"', 'innocent={package="platform-api",version="0.1"}', 'outside={path="../../../outside"}'):
            path.write_text(base+'[dependencies]\n'+dep+'\n')
            with self.subTest(dep=dep), self.assertRaises(ValueError): validate(self.root)

if __name__ == "__main__":
    unittest.main()
