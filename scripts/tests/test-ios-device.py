#!/usr/bin/env python3
"""Stage only built SDK artifacts and run the independent iOS device contract suite."""
import argparse, hashlib, json, pathlib, shutil, subprocess
P = pathlib.Path
SDK = P(__file__).resolve().parents[2]
p = argparse.ArgumentParser(description=__doc__)
for name in ['native-artifact','ffi-artifact','rootfs-artifact','output']:
    p.add_argument('--'+name, type=P, required=True)
p.add_argument('--device', required=True)
p.add_argument('--team', required=True)
p.add_argument('--mode', choices=['app','xctest'], default='xctest')
a = p.parse_args()
subprocess.run(['python3', str(SDK/'scripts/checks/verify-ios-native.py'), '--artifact-dir', str(a.native_artifact)], check=True)
ffi = json.loads((a.ffi_artifact/'ffi-build.json').read_text())
if ffi['platform'] != 'ios' or ffi['native_support_embedded'] or not ffi['abi_metadata_verified']:
    raise SystemExit('expected verified iOS FFI artifact without embedded native support')
for name, expected in ffi['files'].items():
    path = a.ffi_artifact/name
    if not path.resolve().is_relative_to(a.ffi_artifact.resolve()) or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise SystemExit('FFI artifact hash mismatch: '+name)
manifest = json.loads((a.rootfs_artifact/'manifest.json').read_text())
archive = a.rootfs_artifact/'resources/alpine-rootfs.zip'
if hashlib.sha256(archive.read_bytes()).hexdigest() != manifest['rootfs_zip_sha256']:
    raise SystemExit('rootfs artifact hash mismatch')
a.output.mkdir(parents=True, exist_ok=True)
package = a.output/'ios'; example = a.output/'examples/ios'
shutil.copytree(SDK/'ios', package, dirs_exist_ok=True, ignore=shutil.ignore_patterns('Artifacts','NativeSupport','Tests','.build'))
shutil.copytree(SDK/'examples/ios', example, dirs_exist_ok=True, ignore=shutil.ignore_patterns('*.xcodeproj','*.xcworkspace','DeviceResources','.build'))
for name, source in [('MobileLinuxNativeSupport',a.native_artifact),('MobileLinuxRuntimeFFI',a.ffi_artifact)]:
    destination = package/'Artifacts'/(name+'.xcframework')
    if destination.exists(): shutil.rmtree(destination)
    shutil.copytree(source/(name+'.xcframework'), destination)
(package/'Bindings').mkdir(exist_ok=True)
shutil.copy2(a.ffi_artifact/'swift/MobileLinuxRuntimeBindings.swift', package/'Bindings/MobileLinuxRuntimeBindings.swift')
resources=example/'DeviceResources';resources.mkdir(exist_ok=True)
shutil.copy2(archive,resources/archive.name)
shutil.copy2(a.rootfs_artifact/'manifest.json',resources/'manifest.json')
shutil.copytree(a.rootfs_artifact/'resources/RootfsPatch.bundle',resources/'RootfsPatch.bundle',dirs_exist_ok=True)
(resources/'default_mount').mkdir(exist_ok=True)
subprocess.run(['xcodegen','generate'],cwd=example,check=True)
subprocess.run(['xcodebuild','test' if a.mode == 'xctest' else 'build','-project',str(example/'MobileLinuxSample.xcodeproj'),'-scheme','MobileLinuxSample','-destination','platform=iOS,id='+a.device,'-derivedDataPath',str(a.output/'DerivedData'),'-allowProvisioningUpdates','-allowProvisioningDeviceRegistration','DEVELOPMENT_TEAM='+a.team,'CODE_SIGN_STYLE=Automatic'],check=True)

if a.mode == 'app':
    app = a.output/'DerivedData/Build/Products/Debug-iphoneos/MobileLinuxSample.app'
    subprocess.run(['xcrun','devicectl','device','install','app','--device',a.device,str(app)],check=True)
    process = subprocess.Popen(['xcrun','devicectl','device','process','launch','--device',a.device,'--console','--terminate-existing','--timeout','600','org.mobilelinux.sdk.sample','--sdk-device-smoke'],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
    passed = False
    for line in process.stdout:
        print(line,end='',flush=True)
        if line.strip() == 'SDK_SMOKE_RESULT PASS': passed = True
    if process.wait() != 0 or not passed:
        raise SystemExit('physical SDK smoke did not report successful completion')
