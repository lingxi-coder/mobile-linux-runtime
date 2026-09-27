#!/usr/bin/env python3
"""Reproducible iSH native support. All mutable work is confined to explicit cache/output."""
import argparse, hashlib, json, os, pathlib, plistlib, shutil, subprocess, sys, tarfile, tempfile
sys.dont_write_bytecode = True
from sdk_artifact_identity import source_identity
P=pathlib.Path
SDK=P(__file__).resolve().parents[1]
PINS=SDK/'native/ios/sources.json'
def run(args, **kwargs):
    print('+', ' '.join(map(str,args)),flush=True)
    return subprocess.run(list(map(str,args)),check=True,**kwargs)
def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()
def tool(command): return subprocess.check_output(command,text=True).strip()
def copytree(src,dst): shutil.copytree(src,dst,dirs_exist_ok=True)
def source(name,pins,cache):
    info=pins['sources'][name]; revision=info['revision']; repo=cache/'git'/f'{name}.git'
    repo.parent.mkdir(parents=True,exist_ok=True)
    if not repo.exists(): run(['git','init','--bare',repo])
    exists=subprocess.run(['git',f'--git-dir={repo}','cat-file','-e',revision+'^{commit}'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0
    if not exists: run(['git',f'--git-dir={repo}','fetch','--depth=1',info['url'],revision])
    root=cache/'sources'/f'{name}-{revision}'
    if not (root/'.source-revision').exists():
        root.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryFile() as archive:
            run(['git',f'--git-dir={repo}','archive',revision],stdout=archive);archive.seek(0)
            with tarfile.open(fileobj=archive) as tar: tar.extractall(root,filter='data')
        (root/'.source-revision').write_text(revision)
    return root

def prepare(cache):
    pins=json.loads(PINS.read_text())
    for name,digest in pins['sources']['openminis']['vendored_files'].items():
        if sha(SDK/'native/ios/upstream/openminis'/name)!=digest: raise SystemExit('upstream source hash mismatch: '+name)
    for name,digest in pins['patches'].items():
        if sha(SDK/'native/ios/patches'/name)!=digest: raise SystemExit('patch hash mismatch: '+name)
    key=sha(PINS)[:20]; work=cache/'work'/key;ish=work/'ish';glue=work/'openminis'
    if not (work/'prepared').exists():
        copytree(source('ish',pins,cache),ish)
        for name in ['libapps','libarchive']: copytree(source(name,pins,cache),ish/'deps'/name)
        copytree(SDK/'native/ios/upstream/openminis',glue)
        run(['git','apply','--unidiff-zero',SDK/'native/ios/patches/ish-socket-network-policy.patch'],cwd=ish)
        run(['git','apply',SDK/'native/ios/patches/ish-fakefs-utf8-locale.patch'],cwd=ish)
        run(['git','apply',SDK/'native/ios/patches/ish-task-wakeup-signal.patch'],cwd=ish)
        run(['git','apply',SDK/'native/ios/patches/openminis-raw-stdio.patch'],cwd=glue)
        run(['git','apply',SDK/'native/ios/patches/openminis-generic-environment.patch'],cwd=glue)
        run(['git','apply',SDK/'native/ios/patches/openminis-explicit-overlay.patch'],cwd=glue)
        run(['git','apply',SDK/'native/ios/patches/openminis-explicit-host-state.patch'],cwd=glue)
        (work/'prepared').write_text(sha(PINS))
    return work,ish,glue

def native(output,cache,configuration):
    work,ish,glue=prepare(cache); sdk=tool(['xcrun','--sdk','iphoneos','--show-sdk-path']); build=work/('build-ios-'+configuration)
    cross=work/'ios-cross.txt'
    cross.write_text("[binaries]\nc = ['clang', '-arch', 'arm64', '-isysroot', '"+sdk+"', '-miphoneos-version-min=18.0']\nar = 'ar'\nstrip = 'strip'\npkg-config = 'false'\n[host_machine]\nsystem = 'darwin'\ncpu_family = 'aarch64'\ncpu = 'aarch64'\nendian = 'little'\n[properties]\nneeds_exe_wrapper = true\nsys_root = '"+sdk+"'\n")
    if not (build/'build.ninja').exists(): run(['meson','setup',build,ish,'--cross-file',cross,'--buildtype='+configuration.lower(),'-Dlog=','-Dlog_handler=nslog','-Dkernel=ish','-Dengine=asbestos','-Dguest_arch=arm64'])
    run(['ninja','-C',build,'libish.a','libish_emu.a','libfakefs.a','vdso/arm64/libvdso.so.elf'])
    for part in ['lib','include/ish','resources']: (output/part).mkdir(parents=True,exist_ok=True)
    for name in ['libish.a','libish_emu.a','libfakefs.a']: shutil.copy2(build/name,output/'lib'/name)
    for path in ish.rglob('*.h'):
        if any(x.startswith('build') for x in path.relative_to(ish).parts): continue
        dest=output/'include/ish'/path.relative_to(ish);dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(path,dest)
    for path in build.glob('*.h'): shutil.copy2(path,output/'include/ish'/path.name)
    shutil.copy2(build/'vdso/arm64/libvdso.so.elf',output/'resources/libvdso.so.elf')
    copytree(ish/'app/RootfsPatch.bundle',output/'resources/RootfsPatch.bundle')
    # Explicit archive-only correction from the existing app build, preserving source.
    with tempfile.TemporaryDirectory(dir=cache) as temporary:
        temp=P(temporary);run(['ar','-x',output/'lib/libish_emu.a'],cwd=temp)
        member=temp/'asbestos_guest-arm64_gadgets-aarch64_bits.S.o'
        names=temp/'duplicate-gadgets.txt';names.write_text('_gadget_sxtw\n_gadget_uxtb\n_gadget_uxth\n_gadget_rev32\n')
        run(['xcrun','nmedit','-R',names,member])
        run(['xcrun','libtool','-static','-o',output/'lib/libish_emu.a',*temp.glob('*.o')])
    (output/'source-manifest.json').write_text(json.dumps({'source_manifest_sha256':sha(PINS),'pins':json.loads(PINS.read_text()),'configuration':configuration},indent=2)+'\n')
    return work,ish,glue

def native_input_hashes():
    hashes={str(p.relative_to(SDK)):sha(p) for base in [SDK/'ios/NativeSupport',SDK/'native/ios'] for p in base.rglob('*') if p.is_file()}
    for name in ['ios_native_build.py','sdk_artifact_identity.py','build-ios-native.sh','build-ios-xcframework.sh']:
        hashes['scripts/'+name]=sha(SDK/'scripts'/name)
    return hashes

def framework(output,cache,configuration,simulator_only):
    output.mkdir(parents=True,exist_ok=True);cache.mkdir(parents=True,exist_ok=True)
    revision,dirty=source_identity(SDK)
    source_hashes=native_input_hashes()
    if simulator_only: work,ish,glue=prepare(cache)
    else: work,ish,glue=native(output/'native',cache,configuration)
    specs=[('iphonesimulator','arm64','arm64-apple-ios18.0-simulator'),('iphonesimulator','x86_64','x86_64-apple-ios18.0-simulator')]
    if not simulator_only:specs.insert(0,('iphoneos','arm64','arm64-apple-ios18.0'))
    libraries=[]
    for platform,arch,triple in specs:
        target=output/'slices'/triple;fw=target/'MobileLinuxNativeSupport.framework';headers=fw/'Headers';modules=fw/'Modules';objects=target/'objects'
        for p in [headers,modules,objects]:p.mkdir(parents=True,exist_ok=True)
        for name in ['LXISHNativeBridge.h','LXISHKernelBridge.h','LXISHShellExecutorBridge.h','LXISHExecutionPolicy.h']:shutil.copy2(SDK/'ios/NativeSupport'/name,headers/name)
        (headers/'MobileLinuxNativeSupport.h').write_text('#import "LXISHNativeBridge.h"\n#import "LXISHKernelBridge.h"\n#import "LXISHShellExecutorBridge.h"\n#import "LXISHExecutionPolicy.h"\n')
        (headers/'module.modulemap').write_text('module MobileLinuxNativeSupport { umbrella header "MobileLinuxNativeSupport.h" export * }\n')
        (modules/'module.modulemap').write_text('framework module MobileLinuxNativeSupport { umbrella header "MobileLinuxNativeSupport.h" export * }\n')
        sdk=tool(['xcrun','--sdk',platform,'--show-sdk-path']);include=['-I',headers,'-I',output/'native/include','-I',output/'native/include/ish','-I',glue/'src/ios/iSH','-I',glue/'src/ios/NativeOffloads']
        for path in [*sorted((SDK/'ios/NativeSupport').glob('*.m')),*sorted((SDK/'ios/NativeSupport').glob('*.c'))]:
            args=['xcrun','clang','-target',triple,'-isysroot',sdk,'-fblocks',*include,'-c',path,'-o',objects/(path.name+'.o')]
            if path.suffix=='.m':args+=['-fobjc-arc']
            if platform=='iphoneos':args+=['-DISH_INTERNAL=1','-DGUEST_ARM64=1']
            run(args)
        swiftmodule=modules/'MobileLinuxNativeSupport.swiftmodule';swiftmodule.mkdir(exist_ok=True)
        run(['xcrun','swiftc','-parse-as-library','-whole-module-optimization','-emit-object','-emit-module','-enable-library-evolution','-module-name','MobileLinuxNativeSupport','-import-underlying-module','-I',headers,'-target',triple,'-sdk',sdk,'-module-cache-path',cache/'swift-modules','-emit-module-path',swiftmodule/(triple+'.swiftmodule'),'-emit-module-interface-path',swiftmodule/(triple+'.swiftinterface'),*sorted((SDK/'ios/NativeSupport').glob('*.swift')),'-o',objects/'native-swift.o'])
        inputs=list(objects.glob('*.o'))
        if platform=='iphoneos':inputs+=list((output/'native/lib').glob('*.a'))
        run(['xcrun','libtool','-static','-o',fw/'MobileLinuxNativeSupport',*inputs])
        (fw/'Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier':'org.mobile-linux.NativeSupport','CFBundleName':'MobileLinuxNativeSupport','CFBundleExecutable':'MobileLinuxNativeSupport','CFBundlePackageType':'FMWK','CFBundleVersion':'1','CFBundleShortVersionString':'0.1.0','MinimumOSVersion':'18.0','CFBundleSupportedPlatforms':['iPhoneOS' if platform=='iphoneos' else 'iPhoneSimulator']}))
        libraries.append((platform,arch,fw))
    # xcodebuild takes one fat framework per platform variant.
    sim=[entry[2] for entry in libraries if entry[0]=='iphonesimulator'];fat=output/'fat-simulator/MobileLinuxNativeSupport.framework'
    if fat.exists():shutil.rmtree(fat)
    copytree(sim[0],fat)
    run(['xcrun','lipo','-create',*[f/'MobileLinuxNativeSupport' for f in sim],'-output',fat/'MobileLinuxNativeSupport'])
    copytree(sim[1]/'Modules/MobileLinuxNativeSupport.swiftmodule',fat/'Modules/MobileLinuxNativeSupport.swiftmodule')
    result=output/'MobileLinuxNativeSupport.xcframework'
    if result.exists():shutil.rmtree(result)
    args=['xcodebuild','-create-xcframework']
    for platform,arch,fw in libraries:
        if platform=='iphoneos':args+=['-framework',fw]
    args+=['-framework',fat,'-output',result];run(args)
    licenses=output/'licenses';licenses.mkdir(exist_ok=True)
    for source,name in [(SDK/'native/ios/upstream/openminis/LICENSE','OpenMinis-LICENSE'),(ish/'LICENSE.md','iSH-LICENSE.md'),(ish/'LICENSE.IOS','iSH-LICENSE.IOS'),(ish/'deps/libapps/LICENSE','libapps-LICENSE'),(ish/'deps/libarchive/COPYING','libarchive-COPYING')]:
        shutil.copy2(source,licenses/name)
    provenance=output/'source-provenance';provenance.mkdir(exist_ok=True)
    shutil.copy2(PINS,provenance/'sources.json')
    copytree(SDK/'native/ios/patches',provenance/'patches')
    (provenance/'README.txt').write_text('This native bundle combines OpenMinis, iSH, libapps and libarchive. Their original licenses and notices are supplied in ../licenses; no global relicensing is implied. sources.json identifies immutable corresponding-source revisions and the accompanying patches used by this build. SDK-owned source is identified by source_revision in native-support-manifest.json.\n')
    artifacts={str(p.relative_to(output)):sha(p) for base in [result,licenses,provenance] for p in base.rglob('*') if p.is_file()}
    (output/'native-support-manifest.json').write_text(json.dumps({'schema_version':1,'kind':'native-support','contains_rust':False,'source_revision':revision,'source_dirty':dirty,'source_file_sha256':source_hashes,'source_manifest_sha256':sha(PINS),'configuration':configuration,'slices':[entry[0]+'-'+entry[1] for entry in libraries],'artifacts':artifacts},indent=2)+'\n')

def rootfs(archive,output,cache,native_output,profile,expected_sha256):
    output.mkdir(parents=True,exist_ok=True)
    run(['python3',SDK/'scripts/mobile-linux/verify-rootfs-profile.py','--archive',archive,'--arch','aarch64','--profile',profile,'--sha256',expected_sha256,'--receipt',output/'archive-verification.json'])
    work,ish,glue=prepare(cache);build=work/'build-host';env=os.environ.copy();env.pop('IPHONEOS_DEPLOYMENT_TARGET',None);env['LC_ALL']='en_US.UTF-8'
    if not (build/'build.ninja').exists():run(['meson','setup',build,ish,'--buildtype=release','-Dlog=','-Dkernel=ish','-Dengine=asbestos','-Dguest_arch=arm64'],env=env)
    run(['ninja','-C',build,'tools/fakefsify'],env=env)
    output.mkdir(parents=True,exist_ok=True);resources=output/'resources';resources.mkdir(exist_ok=True)
    dest=resources/'alpine-rootfs'
    if dest.exists():shutil.rmtree(dest)
    run([build/'tools/fakefsify',archive,dest],env=env)
    (resources/'default_mount').mkdir(exist_ok=True)
    for name in ['RootfsPatch.bundle','libvdso.so.elf']:
        src=native_output/'resources'/name
        if src.is_dir():copytree(src,resources/name)
        else:shutil.copy2(src,resources/name)
    zip_path=resources/'alpine-rootfs.zip'
    if zip_path.exists():zip_path.unlink()
    run(['ditto','-c','-k','--sequesterRsrc','--keepParent',dest,zip_path])
    # Keep app metadata out of the SDK artifact; caller may add a profile layer.
    (output/'manifest.json').write_text(json.dumps({'schema_version':1,'platform':'ios','guest_abi':'aarch64','rootfs_zip_sha256':sha(zip_path),'source_archive_sha256':sha(archive),'source_manifest_sha256':sha(PINS),'resources':{'rootfs_zip':str(zip_path),'rootfs_dir':str(dest),'default_mount':str(resources/'default_mount'),'vdso':str(resources/'libvdso.so.elf')}},indent=2)+'\n')

def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('action',choices=['native','framework','rootfs']);parser.add_argument('--output',type=P,required=True);parser.add_argument('--cache',type=P,required=True);parser.add_argument('--configuration',choices=['Debug','Release'],default='Release');parser.add_argument('--simulator-only',action='store_true');parser.add_argument('--kind',choices=['native-support'],default='native-support');parser.add_argument('--archive',type=P);parser.add_argument('--native-output',type=P);parser.add_argument('--profile',choices=['base','toolchain'],default='toolchain');parser.add_argument('--expected-archive-sha256');args=parser.parse_args();args.output=args.output.resolve();args.cache=args.cache.resolve();args.cache.mkdir(parents=True,exist_ok=True)
    if args.action=='native':native(args.output,args.cache,args.configuration)
    elif args.action=='framework':framework(args.output,args.cache,args.configuration,args.simulator_only)
    else:
        if args.archive is None or args.native_output is None or args.expected_archive_sha256 is None:parser.error('rootfs requires --archive, --native-output and --expected-archive-sha256')
        rootfs(args.archive.resolve(),args.output,args.cache,args.native_output.resolve(),args.profile,args.expected_archive_sha256)
if __name__=='__main__':main()
