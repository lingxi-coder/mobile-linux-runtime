//! NDK-compiles the vendored mksh + toybox C sources into standalone ELF
//! *executables* for Android targets, then emits their paths via `cargo:`
//! metadata (`cargo:mksh_bin=`, `cargo:toybox_bin=`) for the P5 packaging step.
//!
//! Why direct `clang` invocations (not `cc::Build`)? `cc::Build` produces a
//! static archive to *link into* the Rust crate; we need free-standing
//! executables to `execve` on-device. So this script resolves the NDK target
//! clang and drives it itself — the same `std::process::Command` shape the
//! sibling `android-minijail/build.rs` uses for its table generators.
//!
//! Why the checked-in `third_party/toybox/generated/*`? toybox's own
//! `scripts/make.sh` is GNU-sed + bash only and cannot run from a macOS host
//! (BSD sed errors out — the same trap libminijail hit). Its `generated/`
//! headers are target-arch-independent, so they are pre-generated and vendored
//! (see `third_party/toybox/generated/README.lingxi.md`); this script only does
//! the cross-`cc` pass. mksh's `Build.sh` is a configure script that *runs*
//! compiled probe binaries, which is impossible when cross-compiling from a
//! different host arch — so we use AOSP's fixed `Android.bp` flag set instead.
//!
//! Host builds are a no-op so the workspace stays checkable on macOS/Linux.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Minimum Android API level (the P0a/minijail floor): bionic features and the
/// W^X execve story both assume API 29. The NDK ships per-API clang wrappers
/// (`<arch>-linux-android29-clang`) that bake `--target=<triple>29` in, so
/// pinning the wrapper is enough — no CFLAGS rewriting needed here.
const MIN_ANDROID_API: u32 = 29;

/// The exact toybox C flags AOSP / `scripts/make.sh` use (the `BUILD=` line in
/// a `make` run), minus host-only bits. Order/content mirror upstream so the
/// vendored `generated/*.h` match what these flags expect.
const TOYBOX_CFLAGS: &[&str] = &[
    "-Wall",
    "-Wno-char-subscripts",
    "-Wno-pointer-sign",
    "-funsigned-char",
    "-Wno-deprecated-declarations",
    "-Wno-string-plus-int",
    "-Wno-invalid-source-encoding",
    "-Os",
    "-ffunction-sections",
    "-fdata-sections",
    "-fno-asynchronous-unwind-tables",
    "-fno-strict-aliasing",
];

/// mksh sources (AOSP `Android.bp` `srcs`): the 16-file core build. bionic
/// provides `strlcpy`, so `strlcpy.c` is excluded; `os2.c`/`jehanne.c` are
/// other-OS shims.
const MKSH_SOURCES: &[&str] = &[
    "lalloc.c",
    "edit.c",
    "eval.c",
    "exec.c",
    "expr.c",
    "funcs.c",
    "histrap.c",
    "jobs.c",
    "lex.c",
    "main.c",
    "misc.c",
    "shf.c",
    "syn.c",
    "tree.c",
    "ulimit.c",
    "var.c",
];

/// mksh C defines, copied verbatim from AOSP `Android.bp` `cflags` (the
/// `HAVE_*` set is what `Build.sh` resolves on Android — replicating it lets us
/// skip Build.sh's run-the-probe-binary configure step, impossible when
/// cross-compiling). The `MKSH_DEFAULT_*`/`MKSHRC_PATH` path defines are
/// intentionally omitted: those are AOSP-rootfs paths irrelevant to a bundled
/// APK helper, and mksh has sane built-in fallbacks.
const MKSH_DEFINES: &[&str] = &[
    "-DMKSH_ASSUME_UTF8",
    "-DMKSH_DONT_EMIT_IDSTRING",
    "-DMKSH_BUILDSH",
    "-D_GNU_SOURCE",
    "-DSETUID_CAN_FAIL_WITH_EAGAIN",
    "-DHAVE_STRING_POOLING=2",
    "-DHAVE_ATTRIBUTE_BOUNDED=1",
    "-DHAVE_ATTRIBUTE_FORMAT=1",
    "-DHAVE_ATTRIBUTE_NORETURN=1",
    "-DHAVE_ATTRIBUTE_PURE=1",
    "-DHAVE_ATTRIBUTE_UNUSED=1",
    "-DHAVE_ATTRIBUTE_USED=1",
    "-DHAVE_SYS_TIME_H=1",
    "-DHAVE_TIME_H=1",
    "-DHAVE_BOTH_TIME_H=1",
    "-DHAVE_SYS_BSDTYPES_H=0",
    "-DHAVE_SYS_FILE_H=1",
    "-DHAVE_SYS_MKDEV_H=0",
    "-DHAVE_SYS_MMAN_H=1",
    "-DHAVE_SYS_PARAM_H=1",
    "-DHAVE_SYS_RESOURCE_H=1",
    "-DHAVE_SYS_SELECT_H=1",
    "-DHAVE_SYS_SYSMACROS_H=1",
    "-DHAVE_BSTRING_H=0",
    "-DHAVE_GRP_H=1",
    "-DHAVE_IO_H=0",
    "-DHAVE_LIBGEN_H=1",
    "-DHAVE_LIBUTIL_H=0",
    "-DHAVE_PATHS_H=1",
    "-DHAVE_STDINT_H=1",
    "-DHAVE_STRINGS_H=1",
    "-DHAVE_TERMIOS_H=1",
    "-DHAVE_ULIMIT_H=0",
    "-DHAVE_VALUES_H=0",
    "-DHAVE_CAN_INTTYPES=1",
    "-DHAVE_CAN_UCBINTS=1",
    "-DHAVE_CAN_INT8TYPE=1",
    "-DHAVE_CAN_UCBINT8=1",
    "-DHAVE_SIG_T=1",
    "-DHAVE_SYS_ERRLIST=0",
    "-DHAVE_SYS_SIGNAME=1",
    "-DHAVE_SYS_SIGLIST=1",
    "-DHAVE_FLOCK=1",
    "-DHAVE_LOCK_FCNTL=1",
    "-DHAVE_RLIMIT=1",
    "-DHAVE_RLIM_T=1",
    "-DHAVE_GETRUSAGE=1",
    "-DHAVE_GETSID=1",
    "-DHAVE_GETTIMEOFDAY=1",
    "-DHAVE_KILLPG=1",
    "-DHAVE_MEMMOVE=1",
    "-DHAVE_MKNOD=0",
    "-DHAVE_MMAP=1",
    "-DHAVE_FTRUNCATE=1",
    "-DHAVE_NICE=1",
    "-DHAVE_REVOKE=0",
    "-DHAVE_SETLOCALE_CTYPE=1",
    "-DHAVE_LANGINFO_CODESET=1",
    "-DHAVE_SELECT=1",
    "-DHAVE_SETRESUGID=1",
    "-DHAVE_SETGROUPS=1",
    "-DHAVE_STRERROR=1",
    "-DHAVE_STRSIGNAL=1",
    "-DHAVE_STRLCPY=1",
    "-DHAVE_FLOCK_DECL=1",
    "-DHAVE_REVOKE_DECL=1",
    "-DHAVE_SYS_ERRLIST_DECL=0",
    "-DHAVE_SYS_SIGLIST_DECL=1",
    "-DHAVE_ST_MTIM=1",
    "-DHAVE_ST_MTIMENSEC=0",
    "-DHAVE_PERSISTENT_HISTORY=0",
    "-DMKSH_BUILD_R=593",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Only Android targets get a real build; host builds emit nothing.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }

    let repo_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .ancestors()
        .nth(2) // platforms/android-shellbin -> platforms -> lingxi-code -> repo root
        .expect("manifest dir has 2 ancestors")
        .to_path_buf();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let cc = resolve_ndk_clang();

    let toybox_bin = build_toybox(&repo_root, &out_dir, &cc);
    let mksh_bin = build_mksh(&repo_root, &out_dir, &cc);

    // T2 (packaging) reads these to copy the executables into jniLibs/<abi>.
    println!("cargo:toybox_bin={}", toybox_bin.display());
    println!("cargo:mksh_bin={}", mksh_bin.display());
}

/// Resolve the NDK target clang. cargo-ndk exports `CC_<triple>`/`TARGET_CC`;
/// if neither is set (e.g. a bare `cargo build --target …`), construct the
/// per-API wrapper path from `ANDROID_NDK_HOME` directly. Pinning the API-29
/// wrapper bakes `--target=<triple>29` in, satisfying the bionic floor without
/// any CFLAGS surgery.
fn resolve_ndk_clang() -> PathBuf {
    let target = env::var("TARGET").unwrap();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    for key in [
        format!("CC_{target}"),
        format!("CC_{}", target.replace('-', "_")),
        "TARGET_CC".to_string(),
    ] {
        if let Ok(val) = env::var(&key) {
            if !val.trim().is_empty() {
                let p = PathBuf::from(val);
                // A bare per-API wrapper already pins API 29+; a plain `clang`
                // would default below the floor, so only trust wrappers.
                if p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains("-linux-android") && n.ends_with("-clang"))
                {
                    return p;
                }
            }
        }
    }

    // Fallback: build the wrapper path under ANDROID_NDK_HOME.
    let ndk = env::var("ANDROID_NDK_HOME")
        .or_else(|_| env::var("ANDROID_NDK_ROOT"))
        .expect(
            "ANDROID_NDK_HOME not set and cargo-ndk did not export CC_<triple>; \
             run via `cargo ndk -t <abi> build -p platform-android-shellbin`",
        );
    let host_tag = ndk_host_tag();
    let triple = match arch.as_str() {
        "aarch64" => "aarch64-linux-android",
        "x86_64" => "x86_64-linux-android",
        "arm" => "armv7a-linux-androideabi",
        "x86" => "i686-linux-android",
        other => panic!("unsupported Android target arch {other}"),
    };
    let wrapper = PathBuf::from(ndk)
        .join("toolchains/llvm/prebuilt")
        .join(host_tag)
        .join("bin")
        .join(format!("{triple}{MIN_ANDROID_API}-clang"));
    assert!(
        wrapper.exists(),
        "NDK clang wrapper not found at {} — wrong ANDROID_NDK_HOME or host tag?",
        wrapper.display()
    );
    wrapper
}

/// The NDK prebuilt host directory name for this build host.
fn ndk_host_tag() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin-x86_64"
    } else if cfg!(target_os = "linux") {
        "linux-x86_64"
    } else if cfg!(target_os = "windows") {
        "windows-x86_64"
    } else {
        panic!("unsupported NDK build host OS");
    }
}

/// Cross-`cc` toybox into one ELF executable using the vendored, pre-generated
/// `generated/*.h` (consumed via `-I.`) and the `toyfiles.list` applet set.
fn build_toybox(repo_root: &Path, out_dir: &Path, cc: &Path) -> PathBuf {
    let src = repo_root.join("third_party/toybox");
    assert!(
        src.join("main.c").exists(),
        "vendored toybox not found at {} — see third_party/toybox",
        src.display()
    );
    let toyfiles_list = src.join("generated/toyfiles.list");
    assert!(
        src.join("generated/config.h").exists() && toyfiles_list.exists(),
        "pre-generated toybox build inputs missing under {}/generated — \
         regenerate per generated/README.lingxi.md",
        src.display()
    );
    println!("cargo:rerun-if-changed={}", src.display());

    // lib/*.c
    let mut sources: Vec<PathBuf> = glob_c(&src.join("lib"));
    sources.push(src.join("main.c"));
    // The locked applet set (paths are relative to the toybox root).
    let list = fs::read_to_string(&toyfiles_list)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", toyfiles_list.display()));
    for line in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        sources.push(src.join(line));
    }
    assert!(
        sources.len() > 150,
        "toyfiles.list yielded only {} sources — locked applet set looks truncated",
        sources.len()
    );

    let out = out_dir.join("toybox");
    let mut cmd = Command::new(cc);
    cmd.args(TOYBOX_CFLAGS)
        .arg("-I.")
        .arg("-DTOYBOX_VERSION=\"0.8.11\"")
        .current_dir(&src) // so `-I.` resolves generated/ via the source tree
        .args(["-Wl,--gc-sections"]);
    // Headers live in generated/; toybox includes them as e.g. "generated/config.h"
    // via `-I.` from the source root (current_dir).
    cmd.args(&sources);
    cmd.args(["-lm", "-llog"]);
    cmd.arg("-o").arg(&out);
    run(cmd, "toybox cross-compile", &out);
    out
}

/// Cross-`cc` mksh into one ELF executable using AOSP's fixed flag/define set.
fn build_mksh(repo_root: &Path, out_dir: &Path, cc: &Path) -> PathBuf {
    let src = repo_root.join("third_party/mksh/src");
    assert!(
        src.join("sh.h").exists(),
        "vendored mksh not found at {} — see third_party/mksh",
        src.display()
    );
    println!("cargo:rerun-if-changed={}", src.display());

    let out = out_dir.join("mksh");
    let mut cmd = Command::new(cc);
    cmd.args([
        "-Wall",
        "-Wno-deprecated-declarations",
        "-fno-asynchronous-unwind-tables",
        "-fno-strict-aliasing",
        "-fwrapv",
        "-Os",
        "-I.",
    ])
    .args(MKSH_DEFINES)
    .current_dir(&src)
    .args(MKSH_SOURCES)
    .arg("-o")
    .arg(&out);
    run(cmd, "mksh cross-compile", &out);
    out
}

/// All `*.c` files directly under `dir`, sorted for deterministic command lines.
fn glob_c(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "c"))
        .collect();
    v.sort();
    v
}

/// Run a compile command, asserting success and that the ELF output appeared.
fn run(mut cmd: Command, what: &str, out: &Path) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn NDK clang for {what}: {e}"));
    assert!(status.success(), "{what} failed with {status}");
    assert!(
        out.exists(),
        "{what} reported success but produced no output at {}",
        out.display()
    );
}
