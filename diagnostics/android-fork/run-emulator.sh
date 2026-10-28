#!/usr/bin/env bash
set -euo pipefail
apk="diagnostics/android-fork/app/build/outputs/apk/debug/app-debug.apk"
adb wait-for-device
adb install -r "$apk"
adb logcat -c
adb shell am start -n io.lingxi.forkdiagnostic/.MainActivity
ended=0
for attempt in {1..90}; do
    if adb logcat -d -s LxForkProbe:I '*:S' | grep -q 'DIAGNOSTIC_END'; then
        ended=1
        break
    fi
    if adb logcat -d -s LxForkProbe:I '*:S' | grep -q 'DIAGNOSTIC_ERROR'; then
        break
    fi
    sleep 2
done
adb logcat -d -s LxForkProbe:I '*:S' | tee "$RUNNER_TEMP/fork-diagnostic.log"
adb shell am force-stop io.lingxi.forkdiagnostic
test "$ended" = 1
test "$(grep -c 'RESULT ' "$RUNNER_TEMP/fork-diagnostic.log")" = 16
! grep -q 'DIAGNOSTIC_ERROR' "$RUNNER_TEMP/fork-diagnostic.log"
# The baseline is the exact pre-fix SDK. Do not mistake collection for a fix:
# require the real syscall, shell, network policy and process-owner results.
grep -Eq 'RESULT baseline-fork exit=1 .*fork errno=38' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT baseline-rootfs exit=2 .*can.t fork' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-fork exit=0 .*fork child-exit=0' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-clone exit=0 .*clone child-exit=0' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-clone-untraced exit=1 .*errno=1 ' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-clone3 exit=1 .*errno=38 ' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-fork-no-proot-seccomp exit=0 .*fork child-exit=0' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-rootfs exit=0 .*FORK_FIRST.*FORK_SECOND' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-rootfs-no-proot-seccomp exit=0 .*FORK_FIRST.*FORK_SECOND' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-rootfs-network-disabled exit=0 .*FORK_FIRST.*FORK_SECOND' "$RUNNER_TEMP/fork-diagnostic.log"
grep -Eq 'RESULT current-owner exit=0 heldPipe=false .*OK \(4 native ownership regressions\)' "$RUNNER_TEMP/fork-diagnostic.log"
echo 'Android fork and ownership diagnostic assertions passed; full product acceptance remains separate.'
