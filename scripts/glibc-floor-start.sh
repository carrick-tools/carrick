#!/usr/bin/env bash
# A linux binary starts on the oldest glibc the release promises and on the
# common systems just below the build runner's (carrick#2218).
#
#   glibc-floor-start.sh <binary>
#
# Runs `<binary> --version` inside each image below, with the binary mounted
# read-only. scripts/glibc-floor.sh reads what the binary asks the loader for;
# this asks the loader. The images, with their glibc:
#
#   almalinux:8    2.28, the floor itself (RHEL 8 family)
#   debian:12      2.36, and every node:*-bookworm image
#   ubuntu:22.04   2.35, and the ubuntu-22.04 GitHub runner
#
# The images match the binary's architecture without a platform flag: each
# leg runs this on a runner of its own architecture.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: glibc-floor-start.sh <binary>" >&2
  exit 2
fi

binary="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
failed=0
for image in almalinux:8 debian:12 ubuntu:22.04; do
  if out="$(docker run --rm -v "$binary:/usr/local/bin/carrick:ro" "$image" \
    /usr/local/bin/carrick --version 2>&1)"; then
    echo "$image: $out"
  else
    echo "::error::the binary does not start in $image"
    printf '%s\n' "$out" | sed 's/^/  | /'
    failed=1
  fi
done

exit "$failed"
