#!/usr/bin/env bash
# Record a fixture's `__llm__/` cassette from ONE real analyzer run, then its
# `__golden__.json` by replaying that cassette through the mock analyzer.
#
#   cargo build --release
#   scripts/record-cassette.sh tests/fixtures/<fixture>
#
# The first scan makes real model calls (paid, owner-approved spend). It needs
# a signed-in CLI. It records the fixture on its own:
#
# - `start-scan` still opens a scan slot, because a CLI credential's model
#   calls are refused without one;
# - the cross-repo read comes from an empty, isolated local directory, so no
#   sibling repo is downloaded and no project has to be named
#   (TeeStorage::download_all_repo_data reads only its local half);
# - nothing is uploaded. `CARRICK_OUTPUT_JSON` makes `should_upload_data`
#   return false and ends the run at the JSON projection, before any upload,
#   type-file or run-log step (src/engine/mod.rs). `CARRICK_SKIP_UPLOAD`
#   also stops TeeStorage's cloud write if the upload were ever reached.
#   TeeStorage writes its local copy BEFORE the cloud one, so an attempted
#   upload leaves a file in the local directory, and the script fails on it.
#
# The second scan is fully mocked and free.
#
# A cassette is keyed by the analysed file's stem, so two files with one stem
# cannot share a fixture; the script refuses rather than overwrite one answer
# with the other. A cassette is never edited by hand afterwards.
set -euo pipefail

fixture="${1:?usage: scripts/record-cassette.sh <fixture dir>}"
bin="${CARRICK_BIN:-target/release/carrick}"
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }
[ -x "$bin" ] || { echo "no scanner binary at $bin (cargo build --release)" >&2; exit 2; }
[ -d "$fixture" ] || { echo "no fixture at $fixture" >&2; exit 2; }

cassette="$fixture/__llm__/analyze-file"
if [ -e "$fixture/__llm__" ]; then
  echo "$fixture/__llm__ already exists; remove it to re-record" >&2
  exit 2
fi

dump="$(mktemp -d)"
store="$(mktemp -d)"
trap 'rm -rf "$dump" "$store"' EXIT

# --- 1. one real run, capturing every analyzer answer -------------------------
# A direct, synchronous scan: the dump is written where the analyzer answers,
# so a dispatched job (answers arriving in a bundle later) would record nothing.
env -u CARRICK_DISPATCH -u CARRICK_ANSWERS -u CARRICK_MOCK_ALL \
  CARRICK_LAPTOP_SCAN=1 \
  CARRICK_LOCAL_STORAGE_DIR="$store" CARRICK_LOCAL_STORAGE_ISOLATE=1 \
  CARRICK_SKIP_UPLOAD=1 CARRICK_OUTPUT_JSON=1 \
  CARRICK_EVAL_DUMP_DIR="$dump" \
  "$bin" --no-cache "$fixture" >/dev/null

# The upload tripwire: every upload path writes this directory first.
if [ -n "$(ls -A "$store")" ]; then
  echo "the recording run wrote an index ($(ls "$store")); it must upload nothing" >&2
  exit 1
fi

shopt -s nullglob
answers=("$dump"/*.json)
if [ "${#answers[@]}" -eq 0 ]; then
  echo "the run sent no file to the analyzer; nothing to record" >&2
  exit 1
fi
# A mock run answers with placeholder targets. Refuse it: a cassette must be a
# model's answer (CARRICK_MOCK_ALL must not be set for step 1).
if grep -l 'api\.example\.com' "${answers[@]}" >/dev/null; then
  echo "an answer carries a mock placeholder target; was CARRICK_MOCK_ALL set?" >&2
  exit 1
fi

mkdir -p "$cassette"
for answer in "${answers[@]}"; do
  path="$(jq -r .file_path "$answer")"
  stem="$(basename "${path%.*}")"
  if [ -e "$cassette/$stem.json" ]; then
    echo "two analysed files share the stem '$stem'; rename one" >&2
    exit 1
  fi
  jq -r .raw_response "$answer" | jq . >"$cassette/$stem.json"
done

# --- 2. the golden: the cassette replayed, no network -------------------------
CARRICK_MOCK_ALL=1 CARRICK_OUTPUT_JSON=1 CARRICK_MOCK_FIXTURE_DIR="$fixture/__llm__/" \
  "$bin" "$fixture" >"$fixture/__golden__.json"

echo "recorded ${#answers[@]} answer(s) into $cassette and wrote $fixture/__golden__.json"
