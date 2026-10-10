#!/usr/bin/env bash
# The glibc a linux binary needs, checked against the floor the release
# promises (carrick#2218).
#
#   glibc-floor.sh <binary>...
#
# Reads every GLIBC_x.y symbol version each binary's dynamic symbol table
# references (`objdump -T`) and fails when the highest one is above
# GLIBC_FLOOR (default 2.28). A binary that references no GLIBC_ version at
# all also fails: that is a static or non-glibc build, or an objdump that read
# nothing, and either way the floor was not checked.
#
# The linux binaries are linked against glibc 2.28 by `cargo zigbuild` with a
# `.2.28` target suffix (release.yml, release-build.yml). A plain `cargo build`
# on a current runner links against that runner's glibc instead, and the binary
# then refuses to start on any older system. This is what catches that.
#
# OBJDUMP names the objdump binary (a stub in scripts/glibc-floor.test.sh).
set -euo pipefail

FLOOR="${GLIBC_FLOOR:-2.28}"
OBJDUMP="${OBJDUMP:-objdump}"

if [ "$#" -eq 0 ]; then
  echo "usage: glibc-floor.sh <binary>..." >&2
  exit 2
fi

failed=0
for binary in "$@"; do
  if ! symbols="$("$OBJDUMP" -T "$binary")"; then
    echo "::error::objdump could not read $binary"
    failed=1
    continue
  fi
  highest="$(printf '%s\n' "$symbols" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -V -u | tail -n 1 || true)"
  if [ -z "$highest" ]; then
    echo "::error::$binary references no GLIBC_ symbol version, so its glibc floor was not checked"
    failed=1
    continue
  fi
  # Above the floor when the floor is not the larger of the two.
  if [ "$(printf '%s\n%s\n' "$FLOOR" "$highest" | sort -V | tail -n 1)" != "$FLOOR" ]; then
    echo "::error::$binary needs GLIBC_$highest, above the GLIBC_$FLOOR floor"
    printf '%s\n' "$symbols" | grep "GLIBC_$highest\b" | sed 's/^/  | /' | head -n 20
    failed=1
    continue
  fi
  echo "$binary: highest glibc symbol version GLIBC_$highest (floor GLIBC_$FLOOR)"
done

exit "$failed"
