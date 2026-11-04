#!/usr/bin/env bash
# Decides whether a push to main starts main's check (carrick-cloud#2916).
#
#   scripts/main-check-trigger.sh <before> <after>
#       prints `dispatch <key>` or `skip <key>`, where <key> is the scanner key
#       of <after> (scripts/scanner-key.sh). `skip` when <before> has the same
#       key; `dispatch` when it differs, or when <before> is all zeros or not a
#       commit this clone has (a new branch, a force push). Exit 2 when <after>
#       has no key.
# Run from the repo root with full history. scripts/main-check-trigger.test.sh
# is the test; .github/workflows/main-check-dispatch.yml is the caller.
set -u
if [ "$#" -ne 2 ]; then
  echo "usage: main-check-trigger.sh <before> <after>" >&2
  exit 2
fi
before=$1
after=$2
here="$(cd "$(dirname "$0")" && pwd)"

after_key=$("$here/scanner-key.sh" "$after") || exit 2
before_key=$("$here/scanner-key.sh" "$before" 2>/dev/null) || before_key=""

if [ -n "$before_key" ] && [ "$before_key" = "$after_key" ]; then
  echo "skip $after_key"
else
  echo "dispatch $after_key"
fi
