#!/usr/bin/env bash
# Discover every top-level executable check-*.sh/*-gate.sh wrapper and run it.
# test_gate_triggers.py independently compares discovery and actual execution.
set -euo pipefail

# Invoke wrappers from the SDK repository root, including absolute-path callers.
cd "$(dirname "$0")/.." || exit 1

shopt -s nullglob
gates=()
non_executable=()
seen=$'\n'
for f in scripts/check-*.sh scripts/*-gate.sh; do
    base="$(basename "$f")"
    [[ "$base" == "check-all.sh" ]] && continue
    [[ -f "$f" ]] || continue
    case "$seen" in
        *$'\n'"$base"$'\n'*) continue ;;
    esac
    seen+="$base"$'\n'
    if [[ ! -x "$f" ]]; then
        non_executable+=("$base")
        continue
    fi
    gates+=("$base")
done

if [[ ${#gates[@]} -eq 0 ]]; then
    echo "check-all: discovered 0 gate scripts under scripts/ — discovery is broken, not the repo" >&2
    exit 1
fi

status=0
if [[ ${#non_executable[@]} -ne 0 ]]; then
    printf 'check-all: matching gate is not executable: %s\n' "${non_executable[@]}" >&2
    status=1
fi

# Stable, deterministic order regardless of glob/filesystem ordering.
sorted_gates=()
while IFS= read -r gate; do
    sorted_gates+=("$gate")
done < <(printf '%s\n' "${gates[@]}" | sort)
gates=("${sorted_gates[@]}")

ran=()
for g in "${gates[@]}"; do
    echo "=== RUNNING: $g ==="
    case "$g" in
        lap-gate.sh)
            # lap-gate is a criterion *library*: every subcommand wants a
            # specific --run/--baseline/--range argument for a specific
            # task under review. There is no "check this repo" mode. The
            # closest thing an unattended trigger can run is the engine's
            # own self-test: it exercises all ten planted criteria in both
            # directions and fails if the judging logic itself regresses.
            if ./scripts/lap-gate.sh selftest 2>&1; then rc=0; else rc=$?; fi
            ;;
        *)
            if ./scripts/"$g" 2>&1; then rc=0; else rc=$?; fi
            ;;
    esac
    echo "=== RESULT: $g exit=$rc ==="
    ran+=("$g")
    if [[ $rc -ne 0 ]]; then
        status=$rc
    fi
done

echo "check-all: ran ${#ran[@]} gate(s): ${ran[*]}"
exit "$status"
