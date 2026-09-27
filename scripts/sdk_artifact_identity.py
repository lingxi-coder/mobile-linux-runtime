"""Strict source and exact file-set validation for previously built SDK artifacts."""
import hashlib, json, pathlib, subprocess

def source_identity(root):
    revision=subprocess.check_output(["git","-C",str(root),"rev-parse","HEAD"],text=True).strip()
    dirty=bool(subprocess.check_output(["git","-C",str(root),"status","--porcelain","--untracked-files=normal"]))
    return revision,dirty

def file_hashes(directory,exclude):
    result={}
    for path in pathlib.Path(directory).rglob("*"):
        if path.is_symlink():raise ValueError(f"artifact symlink is not permitted: {path}")
        if path.is_file() and path.name!=exclude:result[path.relative_to(directory).as_posix()]=hashlib.sha256(path.read_bytes()).hexdigest()
    return result

def validate_artifacts(directory,manifest_name,revision,allow_dirty=False,expected=None):
    directory=pathlib.Path(directory)
    manifest=json.loads((directory/manifest_name).read_text())
    if manifest.get("source_revision")!=revision:raise ValueError(f"{manifest_name}: stale or missing source revision")
    if not isinstance(manifest.get("source_dirty"),bool):raise ValueError(f"{manifest_name}: missing source cleanliness")
    if manifest["source_dirty"] and not allow_dirty:raise ValueError(f"{manifest_name}: dirty validation artifact is not releasable")
    for key,value in (expected or {}).items():
        if manifest.get(key)!=value:raise ValueError(f"{manifest_name}: invalid {key}")
    expected_files=manifest.get("files")
    if not isinstance(expected_files,dict) or not expected_files:raise ValueError(f"{manifest_name}: missing exact file hash inventory")
    actual=file_hashes(directory,manifest_name)
    if actual!=expected_files:raise ValueError(f"{manifest_name}: artifact file set or bytes changed")
    return manifest

def validate_ios_native(directory,framework,revision,allow_dirty=False,source_root=None):
    directory=pathlib.Path(directory);framework=pathlib.Path(framework)
    manifest=json.loads((directory/"native-support-manifest.json").read_text())
    if manifest.get("source_revision")!=revision:raise ValueError("native support: stale or missing source revision")
    if not isinstance(manifest.get("source_dirty"),bool):raise ValueError("native support: missing source cleanliness")
    if manifest["source_dirty"] and not allow_dirty:raise ValueError("native support: dirty validation artifact is not releasable")
    if manifest.get("kind")!="native-support" or manifest.get("contains_rust") is not False:raise ValueError("native support: invalid artifact kind")
    if not {"iphoneos-arm64","iphonesimulator-arm64","iphonesimulator-x86_64"}.issubset(manifest.get("slices",[])):raise ValueError("native support: missing required architecture")
    expected=manifest.get("artifacts")
    if not isinstance(expected,dict) or not expected:raise ValueError("native support: missing artifact hashes")
    for name,digest in expected.items():
        file=directory/name
        if not file.resolve().is_relative_to(directory.resolve()) or file.is_symlink():raise ValueError("native support: unsafe artifact path")
        if hashlib.sha256(file.read_bytes()).hexdigest()!=digest:raise ValueError("native support: artifact bytes changed")
    prefix=framework.relative_to(directory).as_posix()+"/"
    actual={prefix+name:digest for name,digest in file_hashes(framework,"").items()}
    if actual!={name:digest for name,digest in expected.items() if name.startswith(prefix)}:raise ValueError("native support: framework file set changed")
    for scope in ["licenses", "source-provenance"]:
        actual_sidecars={scope+"/"+name:digest for name,digest in file_hashes(directory/scope,"").items()}
        if not actual_sidecars or actual_sidecars!={name:digest for name,digest in expected.items() if name.startswith(scope+"/")}:raise ValueError("native support: missing or changed license/source file set")
    sources=manifest.get("source_file_sha256")
    if not isinstance(sources,dict) or not sources:raise ValueError("native support: missing source input hashes")
    if source_root is not None:
        for name,digest in sources.items():
            if hashlib.sha256((pathlib.Path(source_root)/name).read_bytes()).hexdigest()!=digest:raise ValueError("native support: source input changed")
    return manifest

def validate_swift_binding(binding,ffi_manifest):
    expected=ffi_manifest.get("files",{}).get("swift/MobileLinuxRuntimeBindings.swift")
    if not expected or hashlib.sha256(pathlib.Path(binding).read_bytes()).hexdigest()!=expected:raise ValueError("Swift bindings do not match verified FFI generation")
