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
test "$(grep -c 'RESULT ' "$RUNNER_TEMP/fork-diagnostic.log")" = 13
! grep -q 'DIAGNOSTIC_ERROR' "$RUNNER_TEMP/fork-diagnostic.log"
echo 'Diagnostic collection complete. Outcomes above are observations, not a passing product acceptance.'
