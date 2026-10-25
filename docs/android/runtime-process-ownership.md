# Android runtime process ownership

Every command and PTY invocation supplies the actual host TGID to the pinned
PRoot helper after caller environment overrides. A native watcher checks this
parent relationship before guest launch and throughout the tracer's lifetime.
It watches the TGID rather than using a host-thread parent-death signal: retiring
a Tokio or terminal worker must not terminate a live application.

When the entire host dies, the watcher kills the tracer. PTRACE_O_EXITKILL is
required at the first tracee stop and in every subsequent options update,
including the seccomp fallback. The first child also arms SIGKILL PDEATHSIG and
checks the tracer PID before TRACEME, closing the fork-to-SETOPTIONS window.
Failure to arm either mechanism prevents guest execution. Normal fork/clone
children inherit EXITKILL in the kernel; new sessions do not escape it.

CLONE_UNTRACED is rejected. clone3 returns ENOSYS so libc can use clone: its
shared-memory argument cannot be checked without a concurrent-write race.
Genuine SIGTRAP signals still reach the guest after early options installation.
The watcher polls at 100 ms, which is not a strict cleanup deadline when Android
pauses/freezes the tracer. This is lifecycle ownership, not a filesystem or code
security sandbox. Execution still has access to the LingXi Linux user space.

## Regression evidence

Run `cargo test --locked -p mobile-linux-android --lib` for command/PTY owner
identity and the existing cancellation/resource lifecycle suite. Compile
`scripts/tests/android_runtime_owner_probe.c` as a static Android executable
with the matching NDK and run on a device with the built ARM64 helper:

```
owner-probe /data/local/tmp/<test-directory>/libproot.so /data/local/tmp/<test-directory>
```

Place the matching libproot-loader.so in that test directory. The probe verifies
real SIGTRAP delivery, clone restrictions, launch with an already-lost owner,
creator-thread retirement, and host SIGKILL cleanup of a quiet guest and a
TERM-ignoring child that starts its own session. It does not replace application
APK/JNI pairing or product cold-start and data-preservation acceptance.
