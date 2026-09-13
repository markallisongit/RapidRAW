#!/usr/bin/env bash
# Fails if the fork's diff against upstream touches anything outside the agreed
# integration surface (docs/publish-smugmug-design.md), then prints the size of
# that surface so the figure quoted in the PR can be checked rather than asserted.
#
# Usage: scripts/check-fork-surface.sh [base]    (default: upstream/main)
set -euo pipefail

BASE="${1:-upstream/main}"
ALLOWLIST="$(cd "$(dirname "$0")" && pwd)/fork-surface-allowlist.txt"
EXPORT_PROCESSING="src-tauri/src/export_processing.rs"

cd "$(git rev-parse --show-toplevel)"
git fetch upstream --quiet 2>/dev/null || true

allowed=$(grep -v -e '^[[:space:]]*#' -e '^[[:space:]]*$' "$ALLOWLIST")
merge_base=$(git merge-base "$BASE" HEAD)

# Against the working tree, not HEAD, so uncommitted edits are caught before they land.
changed=$(git diff --name-only "$merge_base")
status=0
surface=()

while IFS= read -r file; do
    [ -z "$file" ] && continue
    case "$file" in
        src-tauri/src/publish/*|src/components/panel/right/publish/*) continue ;;
        # Regenerated from Cargo.toml: allowed, and not counted as surface.
        src-tauri/Cargo.lock) continue ;;
    esac

    if [ "$file" = "$EXPORT_PROCESSING" ]; then
        echo "FAIL: export_processing.rs modified — the design commits to a zero-line diff here." >&2
        status=1
    elif ! grep -Fxq "$file" <<< "$allowed"; then
        echo "FAIL: $file is outside the agreed integration surface." >&2
        status=1
    fi

    # New files never conflict; only edits to files upstream also owns are surface.
    if git cat-file -e "$merge_base:$file" 2>/dev/null; then
        surface+=("$file")
    fi
done <<< "$changed"

echo "--- integration surface: edits to upstream files vs $BASE ---"
if [ ${#surface[@]} -gt 0 ]; then
    git diff --stat=120 "$merge_base" -- "${surface[@]}"
else
    echo " 0 files changed"
fi

[ "$status" -eq 0 ] && echo "OK: diff is within the agreed integration surface."
exit $status
