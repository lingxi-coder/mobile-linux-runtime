//! Build-only crate: NDK-compiles the vendored mksh + toybox sources
//! (`third_party/{mksh,toybox}`) into standalone ELF executables for Android
//! targets. `build.rs` emits the built paths as `cargo:mksh_bin=` /
//! `cargo:toybox_bin=` metadata for the P5b/c APK-packaging step; the produced
//! files are `execve`'d on-device from `nativeLibraryDir` (the W^X-legal path),
//! not loaded as Rust libraries. There is no Rust API. Host builds are a no-op.
#![forbid(unsafe_code)]
