#!/usr/bin/env bash
# Record a fixture's `__llm__/` cassette from ONE real analyzer run, then its
# `__golden__.json` by replaying that cassette through the mock analyzer.
#
#   cargo build --release
#   scripts/record-cassette.sh tests/fixtures/<fixture>
#
# The first scan makes real model calls (paid, owner-approved spend) and
# uploads nothing: CARRICK_OUTPUT_JSON keeps the index off the cloud. It needs
# a signed-in CLI. The second scan is fully mocked and free.
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
trap 'rm -rf "$dump"' EXIT

# --- 1. one real run, capturing every analyzer answer -------------------------
CARRICK_EVAL_DUMP_DIR="$dump" CARRICK_OUTPUT_JSON=1 "$bin" --no-cache "$fixture" >/dev/null

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
