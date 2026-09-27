#!/usr/bin/env python3
"""Package built SDK binaries, checksums, Maven layout and SwiftPM metadata."""
import argparse, hashlib, json, pathlib, shutil, subprocess, zipfile
import sys
sys.dont_write_bytecode = True
from sdk_artifact_identity import validate_artifacts, validate_ios_native, validate_swift_binding
ROOT=pathlib.Path(__file__).resolve().parents[1]
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def archive(tree,destination,prefix="",extras=()):
    with zipfile.ZipFile(destination,"w",zipfile.ZIP_DEFLATED) as out:
        files=[(f,(pathlib.PurePosixPath(prefix)/f.relative_to(tree).as_posix()).as_posix()) for f in sorted(tree.rglob("*"))]
        files += [(f,(pathlib.PurePosixPath(base.name)/f.relative_to(base).as_posix()).as_posix()) for base in extras if base.is_dir() for f in sorted(base.rglob("*"))]
        files += [(base,base.name) for base in extras if base.is_file()]
        for f,name in files:
            if f.is_symlink():raise ValueError(f"release input symlink: {f}")
            if not f.is_file():continue
            info=zipfile.ZipInfo(name,(1980,1,1,0,0,0));info.compress_type=zipfile.ZIP_DEFLATED;info.external_attr=0o100644<<16
            out.writestr(info,f.read_bytes())
def main():
    p=argparse.ArgumentParser(description=__doc__)
    for option in ["android-maven","ios-ffi","ios-native","swift-bindings","output-dir"]:p.add_argument("--"+option,type=pathlib.Path,required=True)
    p.add_argument("--version",required=True);p.add_argument("--release-base-url",required=True)
    p.add_argument("--source-revision",required=True)
    p.add_argument("--allow-dirty-validation",action="store_true")
    p.add_argument("--write-root-package",action="store_true",help="Write release-only root SwiftPM manifest and matching Swift bindings after source binaries are validated")
    a=p.parse_args();revision=subprocess.check_output(["git","-C",str(ROOT),"rev-parse","HEAD"],text=True).strip()
    if a.source_revision!=revision:raise ValueError("source revision must be the checkout used to build this SDK")
    dirty=bool(subprocess.check_output(["git","-C",str(ROOT),"status","--porcelain","--untracked-files=normal"]))
    if dirty and not a.allow_dirty_validation:raise ValueError("release requires a clean committed source checkout")
    for xc in [a.ios_ffi,a.ios_native]:
        if not (xc/"Info.plist").is_file():raise ValueError(f"missing real XCFramework {xc}")
    ffi_manifest=validate_artifacts(a.ios_ffi.parent,"ffi-build.json",revision,a.allow_dirty_validation,{"platform":"ios","namespace":"mobile_linux_runtime","native_support_embedded":False,"abi_metadata_verified":True})
    native_manifest=validate_ios_native(a.ios_native.parent,a.ios_native,revision,a.allow_dirty_validation,ROOT)
    if not a.allow_dirty_validation and (ffi_manifest.get("profile")!="release" or native_manifest.get("configuration")!="Release"):
        raise ValueError("Release packaging requires release-profile iOS artifacts")
    subprocess.run(["python3",str(ROOT/"scripts/verify-ios-native.py"),"--artifact-dir",str(a.ios_native.parent)],check=True)
    bindings=a.swift_bindings/"MobileLinuxRuntimeBindings.swift"
    validate_swift_binding(bindings,ffi_manifest)
    maven_manifest=validate_artifacts(a.android_maven,"sdk-artifacts.json",revision,a.allow_dirty_validation,{"version":a.version,"native_support_only":False})
    if maven_manifest.get("validation_only") is not False and not a.allow_dirty_validation:raise ValueError("Development-only Maven publication cannot be released")
    out=a.output_dir.resolve();out.mkdir(parents=True,exist_ok=True)
    names={"ffi":f"MobileLinuxRuntimeFFI-{a.version}.xcframework.zip","native":f"MobileLinuxNativeSupport-{a.version}.xcframework.zip","maven":f"mobile-linux-maven-{a.version}.zip"}
    archive(a.ios_ffi,out/names["ffi"],"MobileLinuxRuntimeFFI.xcframework")
    archive(a.ios_native,out/names["native"],"MobileLinuxNativeSupport.xcframework",[a.ios_native.parent/"licenses",a.ios_native.parent/"source-provenance",a.ios_native.parent/"native-support-manifest.json"])
    archive(a.android_maven,out/names["maven"])
    package=out/"swift-package";package.mkdir(exist_ok=True)
    shutil.copytree(ROOT/"ios/SDK",package/"ios/SDK",dirs_exist_ok=True)
    shutil.copytree(ROOT/"ios/NativeSupportLink",package/"ios/NativeSupportLink",dirs_exist_ok=True)
    (package/"ios/Bindings").mkdir(parents=True,exist_ok=True);shutil.copy2(bindings,package/"ios/Bindings"/bindings.name)
    manifest=(ROOT/"ios/Package.swift").read_text()
    for kind,target in [("ffi","MobileLinuxRuntimeFFI"),("native","MobileLinuxNativeSupport")]:
        manifest=manifest.replace(f'path: "Artifacts/{target}.xcframework"',f'url: "{a.release_base_url.rstrip("/")}/{names[kind]}", checksum: "{sha(out/names[kind])}"')
    manifest=manifest.replace('path: "Bindings"','path: "ios/Bindings"').replace('path: "SDK"','path: "ios/SDK"').replace('path: "NativeSupportLink"','path: "ios/NativeSupportLink"')
    (package/"Package.swift").write_text(manifest)
    archive(package,out/f"mobile-linux-swift-package-{a.version}.zip")
    if a.write_root_package:
        if a.allow_dirty_validation:raise ValueError("The public root SwiftPM manifest requires release-quality inputs")
        (ROOT/"Package.swift").write_text(manifest)
        (ROOT/"ios/Bindings").mkdir(exist_ok=True);shutil.copy2(bindings,ROOT/"ios/Bindings"/bindings.name)
    archives={f.name:sha(f) for f in sorted(out.glob("*.zip"))}
    (out/"SHA256SUMS").write_text("".join(f"{digest}  {name}\n" for name,digest in archives.items()))
    (out/"release-provenance.json").write_text(json.dumps({"schema_version":1,"version":a.version,"source_revision":revision,"validation_only":a.allow_dirty_validation or dirty or ffi_manifest["source_dirty"] or native_manifest["source_dirty"] or maven_manifest["source_dirty"],"swift_package_source_revision":None,"archives":archives,"note":"The wrapper/package commit may follow this binary source commit; record it when publishing. Rootfs assets are supplied explicitly by each embedding application."},indent=2)+"\n")
    print(out/"release-provenance.json")
if __name__=="__main__":main()
