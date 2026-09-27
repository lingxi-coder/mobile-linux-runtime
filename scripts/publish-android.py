#!/usr/bin/env python3
"""Build AARs into a caller-owned Maven repository, without writing SDK source directories."""
import argparse, hashlib, json, os, pathlib, subprocess, zipfile
import sys
sys.dont_write_bytecode = True
from sdk_artifact_identity import source_identity, validate_artifacts
from release_notices import verify as verify_notices, FILES as NOTICE_FILES
ROOT=pathlib.Path(__file__).resolve().parents[1]
def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument("--native-artifacts",type=pathlib.Path,required=True)
    p.add_argument("--ffi-artifacts",type=pathlib.Path)
    p.add_argument("--maven-dir",type=pathlib.Path,required=True)
    p.add_argument("--build-dir",type=pathlib.Path,required=True)
    p.add_argument("--version",default="0.1.0")
    p.add_argument("--native-only",action="store_true")
    p.add_argument("--offline",action="store_true")
    p.add_argument("--allow-dirty-validation",action="store_true")
    a=p.parse_args()
    if not a.native_only and a.ffi_artifacts is None:p.error("full SDK requires --ffi-artifacts")
    revision,dirty=source_identity(ROOT)
    if dirty and not a.allow_dirty_validation:raise ValueError("SDK publication requires clean committed sources; use --allow-dirty-validation only for development")
    native_manifest=validate_artifacts(a.native_artifacts.resolve(),"native-manifest.json",revision,a.allow_dirty_validation,{"kind":"native-support-only","contains_rust_core":False})
    ffi_manifest=None
    if not a.native_only:
        ffi_manifest=validate_artifacts(a.ffi_artifacts.resolve(),"ffi-build.json",revision,a.allow_dirty_validation,{"platform":"android","namespace":"mobile_linux_runtime","native_support_embedded":False,"abi_metadata_verified":True})
        verify_notices(ROOT,a.ffi_artifacts/"licenses")
        if not a.allow_dirty_validation and ffi_manifest.get("profile")!="release":raise ValueError("Release AAR requires release-profile Rust FFI artifacts")
        for abi in ("arm64-v8a","x86_64"):
            if f"jniLibs/{abi}/libmobile_linux_runtime.so" not in ffi_manifest["files"]:raise ValueError(f"missing FFI ABI {abi}")
        if not any(name.endswith(".kt") for name in ffi_manifest["files"]):raise ValueError("missing generated Kotlin metadata")
    subprocess.run(["python3",str(ROOT/"scripts/verify-android-native.py"),"--artifact-dir",str(a.native_artifacts.resolve())],check=True)
    build=a.build_dir.resolve();build.mkdir(parents=True,exist_ok=True);maven=a.maven_dir.resolve();maven.mkdir(parents=True,exist_ok=True)
    tasks=[":installer:publishReleasePublicationToMavenRepository",":native-support:publishReleasePublicationToMavenRepository",":gradle-plugin:publish"]
    if not a.native_only:tasks.append(":runtime:publishReleasePublicationToMavenRepository")
    cmd=[str(ROOT/"android/gradlew"),"--project-dir",str(ROOT/"android"),"--project-cache-dir",str(build/"gradle-cache"),"--no-daemon",f"-Pkotlin.project.persistent.dir={build / 'kotlin'}",f"-PsdkVersion={a.version}",f"-PsdkBuildDir={build}",f"-PsdkMavenRepo={maven}",f"-PnativeArtifacts={a.native_artifacts.resolve()}"]
    if a.ffi_artifacts:cmd.append(f"-PffiArtifacts={a.ffi_artifacts.resolve()}")
    if a.offline:cmd.append("--offline")
    subprocess.run(cmd+tasks,check=True)
    if not a.native_only:
        aar=maven/"io/github/lingxi-coder/mobile-linux-runtime"/a.version/f"mobile-linux-runtime-{a.version}.aar"
        with zipfile.ZipFile(aar) as bundle:
            for name,source in NOTICE_FILES.items():
                if bundle.read("assets/"+name)!=(ROOT/source).read_bytes():raise ValueError("Runtime AAR missing original license/notice: "+name)
    if source_identity(ROOT)!=(revision,dirty):raise ValueError("SDK source identity changed during publication")
    files={str(f.relative_to(maven)):hashlib.sha256(f.read_bytes()).hexdigest() for f in maven.rglob("*") if f.is_file() and f.name!="sdk-artifacts.json"}
    (maven/"sdk-artifacts.json").write_text(json.dumps({"version":a.version,"source_revision":revision,"source_dirty":dirty or native_manifest["source_dirty"] or bool(ffi_manifest and ffi_manifest["source_dirty"]),"validation_only":a.allow_dirty_validation,"native_manifest_sha256":hashlib.sha256((a.native_artifacts/"native-manifest.json").read_bytes()).hexdigest(),"ffi_manifest_sha256":hashlib.sha256((a.ffi_artifacts/"ffi-build.json").read_bytes()).hexdigest() if ffi_manifest else None,"native_support_only":a.native_only,"files":files},indent=2)+"\n")
if __name__=="__main__":main()
