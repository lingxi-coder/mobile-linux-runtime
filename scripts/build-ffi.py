#!/usr/bin/env python3
"""Build the standalone FFI libraries and bindings; never build the native helper bundle."""
import argparse, hashlib, json, os, pathlib, shutil, subprocess, tempfile, tomllib
import sys
sys.dont_write_bytecode = True
from sdk_artifact_identity import source_identity
from release_notices import stage as stage_notices, FILES as NOTICE_FILES
ROOT = pathlib.Path(__file__).resolve().parents[1]
def run(args, env): subprocess.run(args, cwd=ROOT, env=env, check=True)
def source_inputs():
    paths=[*ROOT.joinpath("crates").rglob("*.rs"),*ROOT.joinpath("crates").rglob("Cargo.toml"),ROOT/"Cargo.toml",ROOT/"Cargo.lock",ROOT/"rust-toolchain.toml",ROOT/"crates/mobile-linux-ffi/uniffi.toml",pathlib.Path(__file__).resolve()]
    paths += [ROOT / source for source in NOTICE_FILES.values()]
    return {str(path.relative_to(ROOT)):hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("--platform",choices=["android","ios"],required=True)
    p.add_argument("--output-dir",type=pathlib.Path,required=True)
    p.add_argument("--target-dir",type=pathlib.Path,required=True)
    p.add_argument("--release",action="store_true")
    a=p.parse_args();out=a.output_dir.resolve();target=a.target_dir.resolve();out.mkdir(parents=True,exist_ok=True)
    source_revision,source_dirty=source_identity(ROOT)
    source_file_sha256=source_inputs()
    env=dict(os.environ,CARGO_TARGET_DIR=str(target),CARGO_INCREMENTAL="0",CARGO_PROFILE_DEV_DEBUG="0")
    if a.platform=="ios":env["IPHONEOS_DEPLOYMENT_TARGET"]="18.0"
    toolchain=tomllib.loads((ROOT/"rust-toolchain.toml").read_text())["toolchain"]["channel"]
    env["RUSTUP_TOOLCHAIN"]=toolchain
    cargo=["cargo"]
    run(cargo+["build","--locked","-p","mobile-linux-ffi","--lib"],env)
    run(cargo+["build","--locked","-p","mobile-linux-ffi","--features","bindgen","--bin","mobile-linux-bindgen"],env)
    generator=target/"debug/mobile-linux-bindgen"
    library=target/("debug/libmobile_linux_runtime.dylib" if os.uname().sysname=="Darwin" else "debug/libmobile_linux_runtime.so")
    language="kotlin" if a.platform=="android" else "swift"
    generated=out/language;generated.mkdir(exist_ok=True)
    run([str(generator),"generate","--library",str(library),"--language",language,"--config",str(ROOT/"crates/mobile-linux-ffi/uniffi.toml"),"--out-dir",str(generated)],env)
    profile="release" if a.release else "debug";flags=["--release"] if a.release else []
    if a.platform=="android":
        run(cargo+["ndk","-t","arm64-v8a","-t","x86_64","--platform","26","-o",str(out/"jniLibs"),"build","--locked","-p","mobile-linux-ffi","--lib",*flags],env)
        # Read UniFFI metadata from the actual target ELF files. Host-generated
        # Kotlin must describe exactly the same ABI for both architectures.
        kotlin_files={f.relative_to(generated).as_posix():f.read_bytes() for f in generated.rglob("*.kt")}
        for abi in ("arm64-v8a","x86_64"):
            with tempfile.TemporaryDirectory(prefix="mobile-linux-abi-") as temporary:
                run([str(generator),"generate","--library",str(out/"jniLibs"/abi/"libmobile_linux_runtime.so"),"--language","kotlin","--config",str(ROOT/"crates/mobile-linux-ffi/uniffi.toml"),"--out-dir",temporary],env)
                actual={f.relative_to(temporary).as_posix():f.read_bytes() for f in pathlib.Path(temporary).rglob("*.kt")}
                if not kotlin_files or actual!=kotlin_files:raise ValueError(f"Kotlin ABI metadata differs from actual {abi} Rust library")
    else:
        triples=["aarch64-apple-ios","aarch64-apple-ios-sim","x86_64-apple-ios"]
        for triple in triples:
            run(cargo+["rustc","--locked","-p","mobile-linux-ffi","--lib","--crate-type","staticlib","--target",triple,*flags],env)
        swift_files={f.relative_to(generated).as_posix():f.read_bytes() for f in generated.iterdir() if f.is_file()}
        for triple in triples:
            with tempfile.TemporaryDirectory(prefix="mobile-linux-swift-abi-") as temporary:
                run([str(generator),"generate","--library",str(target/triple/profile/"libmobile_linux_runtime.a"),"--language","swift","--config",str(ROOT/"crates/mobile-linux-ffi/uniffi.toml"),"--out-dir",temporary],env)
                actual={f.relative_to(temporary).as_posix():f.read_bytes() for f in pathlib.Path(temporary).iterdir() if f.is_file()}
                if not swift_files or actual!=swift_files:raise ValueError(f"Swift ABI metadata differs from actual {triple} Rust library")
        headers=out/"headers";headers.mkdir(exist_ok=True)
        shutil.copy2(generated/"MobileLinuxRuntimeFFI.h",headers/"MobileLinuxRuntimeFFI.h")
        shutil.copy2(generated/"MobileLinuxRuntimeFFI.modulemap",headers/"module.modulemap")
        simulator=out/"libmobile_linux_runtime_sim.a"
        run(["xcrun","lipo","-create",str(target/triples[1]/profile/"libmobile_linux_runtime.a"),str(target/triples[2]/profile/"libmobile_linux_runtime.a"),"-output",str(simulator)],env)
        xc=out/"MobileLinuxRuntimeFFI.xcframework"
        if xc.exists(): shutil.rmtree(xc)
        run(["xcodebuild","-create-xcframework","-library",str(target/triples[0]/profile/"libmobile_linux_runtime.a"),"-headers",str(headers),"-library",str(simulator),"-headers",str(headers),"-output",str(xc)],env)
    if source_identity(ROOT)!=(source_revision,source_dirty):raise ValueError("SDK source identity changed while building FFI")
    if source_inputs()!=source_file_sha256:raise ValueError("SDK source inputs changed while building FFI")
    stage_notices(ROOT,out/"licenses")
    artifacts={str(f.relative_to(out)):hashlib.sha256(f.read_bytes()).hexdigest() for f in out.rglob("*") if f.is_file() and f.name!="ffi-build.json"}
    (out/"ffi-build.json").write_text(json.dumps({"schema_version":1,"platform":a.platform,"namespace":"mobile_linux_runtime","profile":profile,"toolchain":toolchain,"native_support_embedded":False,"source_revision":source_revision,"source_dirty":source_dirty,"abi_metadata_verified":True,"source_file_sha256":source_file_sha256,"files":artifacts},indent=2)+"\n")

if __name__=="__main__":main()
