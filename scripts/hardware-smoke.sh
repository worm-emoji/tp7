#!/usr/bin/env bash
set -euo pipefail

REMOTE_DIR="${TP7_SMOKE_REMOTE_DIR:-/memo}"
REMOTE_DIR="${REMOTE_DIR%/}"
if [[ -z "$REMOTE_DIR" ]]; then
  REMOTE_DIR="/"
fi

LOCAL_ROOT="${TP7_SMOKE_LOCAL_ROOT:-target/tp7-hardware-smoke}"
BASE="tp7cli-smoke-$(date +%Y%m%d%H%M%S)-$$"
FILE_NAME="$BASE.txt"
DIR_A_NAME="$BASE-dir-a.txt"
DIR_B_NAME="$BASE-dir-b.txt"
PREFLIGHT_FILE_NAME="$BASE-preflight.txt"
PREFLIGHT_MISSING_DIR="$BASE-missing-folder"

remote_child() {
  if [[ "$REMOTE_DIR" == "/" ]]; then
    printf "/%s" "$1"
  else
    printf "%s/%s" "$REMOTE_DIR" "$1"
  fi
}

run_tp7_once() {
  if [[ -n "${TP7_SMOKE_BIN:-}" ]]; then
    "$TP7_SMOKE_BIN" "$@"
  else
    cargo run -- "$@"
  fi
}

is_transient_handoff_error() {
  [[ "$1" == *"No TP-7 devices were found"* ]] ||
    [[ "$1" == *"No TP-7 CoreMIDI source endpoint was found"* ]] ||
    [[ "$1" == *"No TP-7 CoreMIDI destination endpoint was found"* ]]
}

run_tp7() {
  local retries="${TP7_SMOKE_RETRIES:-3}"
  local delay="${TP7_SMOKE_RETRY_DELAY:-3}"
  local attempt=1
  local output
  local status

  while true; do
    set +e
    output=$(run_tp7_once "$@" 2>&1)
    status=$?
    set -e

    printf "%s\n" "$output"

    if [[ "$status" -eq 0 ]]; then
      return 0
    fi

    if [[ "$attempt" -ge "$retries" ]] || ! is_transient_handoff_error "$output"; then
      return "$status"
    fi

    echo "Transient TP-7 handoff; retrying ($attempt/$retries)" >&2
    sleep "$delay"
    attempt=$((attempt + 1))
  done
}

cleanup() {
  (
    set +e
    run_tp7 --auto-connect rm "$(remote_child "$FILE_NAME")" --force >/dev/null 2>&1
    run_tp7 --auto-connect rm "$(remote_child "$DIR_A_NAME")" --force >/dev/null 2>&1
    run_tp7 --auto-connect rm "$(remote_child "$DIR_B_NAME")" --force >/dev/null 2>&1
    run_tp7 --auto-connect rm "$(remote_child "$PREFLIGHT_FILE_NAME")" --force >/dev/null 2>&1
    true
  )
}
trap cleanup EXIT

expect_preflight_failure() {
  local output
  local status

  set +e
  output=$(run_tp7 --auto-connect --no-progress push "$LOCAL_ROOT/preflight" "$REMOTE_DIR" --recursive 2>&1)
  status=$?
  set -e

  printf "%s\n" "$output"

  if [[ "$status" -eq 0 ]]; then
    echo "expected preflight failure for missing remote folder" >&2
    exit 1
  fi

  if [[ "$output" != *"remote folder creation is not available"* ]]; then
    echo "unexpected preflight failure; wanted missing-folder validation" >&2
    exit "$status"
  fi
}

echo "Preparing local smoke files under $LOCAL_ROOT"
rm -rf "$LOCAL_ROOT"
mkdir -p "$LOCAL_ROOT/dir" "$LOCAL_ROOT/preflight/$PREFLIGHT_MISSING_DIR"
# Keep hardware smoke fixtures tiny. Real TP-7 recordings can be multi-GB.
printf "tp7cli smoke first\n" >"$LOCAL_ROOT/$FILE_NAME"
printf "tp7cli directory smoke a\n" >"$LOCAL_ROOT/dir/$DIR_A_NAME"
printf "tp7cli directory smoke b\n" >"$LOCAL_ROOT/dir/$DIR_B_NAME"
printf "tp7cli preflight top\n" >"$LOCAL_ROOT/preflight/$PREFLIGHT_FILE_NAME"
printf "tp7cli preflight nested\n" >"$LOCAL_ROOT/preflight/$PREFLIGHT_MISSING_DIR/nested.txt"

echo "Cleaning old remote smoke files in $REMOTE_DIR"
cleanup

echo "Checking recursive push preflight"
expect_preflight_failure

if run_tp7 --auto-connect stat "$(remote_child "$PREFLIGHT_FILE_NAME")" >/dev/null 2>&1; then
  echo "preflight wrote $(remote_child "$PREFLIGHT_FILE_NAME") before failing" >&2
  exit 1
fi

echo "Pushing file"
run_tp7 --auto-connect --no-progress push "$LOCAL_ROOT/$FILE_NAME" "$(remote_child "$FILE_NAME")"

echo "Overwriting file through folder destination"
printf "tp7cli smoke second and longer\n" >"$LOCAL_ROOT/$FILE_NAME"
run_tp7 --auto-connect --no-progress push "$LOCAL_ROOT/$FILE_NAME" "$REMOTE_DIR/" --overwrite
run_tp7 --auto-connect stat "$(remote_child "$FILE_NAME")"

echo "Pushing directory into existing remote folder"
run_tp7 --auto-connect --no-progress push "$LOCAL_ROOT/dir" "$REMOTE_DIR" --recursive
run_tp7 --auto-connect stat "$(remote_child "$DIR_A_NAME")"
run_tp7 --auto-connect stat "$(remote_child "$DIR_B_NAME")"

echo "Validating clean MTP close"
run_tp7 --auto-connect eject

echo "Hardware smoke passed"
