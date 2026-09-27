# Pre-generated toybox build inputs (LingXi P5a)

The files in this directory are **checked-in build artifacts** that toybox's
`make`/`scripts/make.sh` normally produces on the build host. They are
target-architecture-independent (flag bitmasks, struct layouts, help text,
the applet `NEWTOY` tables — no syscall numbers), so generating them once and
committing them lets `platforms/android-shellbin/build.rs` cross-compile
toybox with a plain NDK `clang` invocation, **without** requiring GNU sed /
bash on every build host (toybox's `scripts/make.sh` is GNU-sed-only and was
the reason the `make` path could not run from a macOS host — see the build.rs
header).

## Files

- `config.h`, `newtoys.h`, `flags.h`, `globals.h`, `help.h`, `tags.h` —
  the headers `scripts/make.sh` emits into `generated/` (consumed via `-I.`).
- `toyfiles.list` — the exact `toys/*/*.c` set enabled by `lingxi.config`
  (one path per line). build.rs compiles `lib/*.c` + `main.c` + this list.

## How to regenerate (only when bumping the toybox version or config)

From a host with GNU sed (`brew install gnu-sed` on macOS):

```sh
cd third_party/toybox
cp lingxi.config .config
rm -rf generated
SED=gsed make CC=cc HOSTCC=cc            # host compile fails late (Linux-only
                                         # headers) but emits generated/*.h first
# copy the headers back into generated/ (drop host-specific build.sh):
#   config.h newtoys.h flags.h globals.h help.h tags.h
# rebuild toyfiles.list:
grep -oE 'toys/[a-z]+/[a-z0-9_]+\.c' generated/build.sh | sort > generated/toyfiles.list
```

## lingxi.config deltas from upstream `make defconfig`

Disabled (bionic has no `<shadow.h>`/`<crypt.h>`; ZHELP needs a working host
gzip build that the macOS host can't provide):

- `CONFIG_SU`, `CONFIG_LOGIN`, `CONFIG_MKPASSWD` — need `crypt()`/`getspnam()`.
- `CONFIG_TOYBOX_ZHELP` — compressed help; plain `help.h` used instead.

Everything else is the locked full defconfig applet set.
