#!/usr/bin/env python3
"""Produce release evidence from a real validated tree/archive/APK closure."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
sys.dont_write_bytecode = True
from source_contract import external_output
import rootfs_tool as tool

parser = argparse.ArgumentParser()
parser.add_argument("--root", type=Path, required=True)
parser.add_argument("--archive", type=Path, required=True)
parser.add_argument("--closure", type=Path, required=True)
parser.add_argument("--abi", choices=("arm64-v8a", "x86_64"), required=True)
parser.add_argument("--output-dir", required=True)
args = parser.parse_args()
output = external_output(args.output_dir, args.root, args.archive, args.closure)
output.mkdir(parents=True, exist_ok=True)
pins = tool._PINS
arch = "aarch64" if args.abi == "arm64-v8a" else "x86_64"
closure = json.loads(args.closure.read_text())
if closure != {"arch":arch, "artifacts":pins["apk_artifacts"][args.abi]["artifacts"]}:
    raise SystemExit("actual APK closure differs from committed source pins")
tool.validate_rootfs_tree(args.root)
tool.verify_archive(argparse.Namespace(archive=args.archive))
digest = tool.read_sha256(args.archive)
manifest = output / "rootfs-manifest.json"
lock = output / "rootfs-build.lock.json"
tool.generate_manifest(argparse.Namespace(root=args.root, runtime="android-proot", platform="android", abi="arm64" if arch == "aarch64" else "x86_64", rootfs_version=pins["alpine"]["version"], archive_filename=args.archive.name, archive_sha256=digest, archive_size=args.archive.stat().st_size, output=manifest))
tool.generate_lock(argparse.Namespace(root=args.root, output=lock))
tool.generate_spdx(argparse.Namespace(root=args.root, name=f"mobile-linux-runtime-{args.abi}-{pins['alpine']['version']}", source_date_epoch=0, output=output / "rootfs.spdx.json"))
tool.snapshot_allowlist(argparse.Namespace(manifest=manifest, output=output / "executable-allowlist.json"))
(output / "apk-closure.json").write_text(json.dumps(closure, indent=2) + "\n")
provenance = {"schema_version":1,"abi":args.abi,"architecture":arch,"rootfs_version":pins["alpine"]["version"],"archive_sha256":digest,"source_toolchain_pins_sha256":tool.read_sha256(tool._PINS_PATH),"apk_closure_sha256":hashlib.sha256(tool.canonical_json_bytes(closure)).hexdigest(),"node_provenance":json.loads((args.root / "opt/lingxi/toolchains/node/provenance.json").read_text()),"producer_input_sha256":{p.name:tool.read_sha256(p) for p in sorted(Path(__file__).parent.iterdir()) if p.is_file()}}
(output / "producer-inputs.json").write_text(json.dumps(provenance, indent=2) + "\n")
subprocess.run([sys.executable,str(Path(__file__).with_name("verify-evidence.py")),"--evidence-dir",str(output),"--root",str(args.root),"--archive",str(args.archive)],check=True)
print(f"release evidence produced from actual {arch} rootfs: {digest}")
