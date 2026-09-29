#!/usr/bin/env bash
#
# Fact-coverage probe (carrick#1556).
#
# Scans a checkout with the model stage switched off and counts the consumer
# rows the deterministic layer states, by the pass that stated them. With no
# model there is no model row: every row printed is one the source states, and
# a change to the deterministic layer shows up here as rows moving between
# sources or appearing where there were none.
#
# Free to run. `CARRICK_NO_MODEL=1` sends nothing to the model and
# `CARRICK_MOCK_ALL=1` keeps the cloud out of it, so the reading costs a scan's
# CPU and nothing else. Run it on the same tree before and after a change and
# diff the two outputs.
#
# Usage:
#   scripts/fact-coverage.sh <checkout>
#
# Environment:
#   CARRICK_BIN   scanner binary (default: target/debug/carrick)
#
# Output: one line per (source, service-relative directory) with a row count,
# then a total. Exit status is the scanner's.

set -uo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
bin="${CARRICK_BIN:-$repo_root/target/debug/carrick}"
target="${1:?usage: scripts/fact-coverage.sh <checkout>}"

if [ ! -x "$bin" ]; then
  echo "no scanner binary at $bin (cargo build first, or set CARRICK_BIN)" >&2
  exit 2
fi

projection="$(mktemp "${TMPDIR:-/tmp}/carrick-fact-coverage.XXXXXX")"
trap 'rm -f "$projection"' EXIT

CARRICK_NO_MODEL=1 \
CARRICK_MOCK_ALL=1 \
CARRICK_OUTPUT_JSON=1 \
CARRICK_SKIP_INTENTS=1 \
  env -u GITHUB_REPOSITORY -u GITHUB_ACTIONS -u CI \
  "$bin" "$target" >"$projection" 2>/dev/null
status=$?
if [ "$status" -ne 0 ]; then
  echo "scanner exited $status on $target" >&2
  exit "$status"
fi

python3 - "$projection" <<'PY'
import collections, json, sys

with open(sys.argv[1]) as handle:
    projection = json.load(handle)

counts = collections.Counter()
for call in projection.get("calls", []):
    source = call.get("resolution_source") or "not stated"
    directory = "/".join(call.get("file", "").split("/")[:2])
    counts[(source, directory)] += 1

for (source, directory), count in sorted(counts.items()):
    print(f"{count:6d}  {source:22s} {directory}")
print(f"{sum(counts.values()):6d}  total")
PY
