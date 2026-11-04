#!/usr/bin/env bash
# Scanner key: one hex string that changes only when the scanner's behaviour can.
#
#   scripts/scanner-key.sh <rev>     prints the key of <rev> (any git rev), exit 0
#                                    exit 2 and a message on stderr when it cannot
#
# The key is a sha256 over:
#   1. `git ls-tree -r` of src, crates and build.rs (mode, blob id, path), leaving
#      out crates/*/Cargo.toml, which are read in 2;
#   2. Cargo.toml, crates/*/Cargo.toml and Cargo.lock with the version of this
#      workspace's own packages removed (release-please bumps those every
#      release, and a version bump alone does not change the scanner).
# A version line is dropped from a Cargo.toml's [package] table, and from a
# Cargo.lock package that has no `source` (a path package of this workspace).
# scripts/release-diff-check.py applies the same rule to decide whether a diff
# is version-only; scripts/scanner-key.test.sh keeps the two in step.
set -u

if [ "$#" -ne 1 ]; then
  echo "usage: scanner-key.sh <rev>" >&2
  exit 2
fi
rev=$1
git rev-parse --verify --quiet "$rev^{commit}" >/dev/null || {
  echo "scanner-key: unknown revision: $rev" >&2
  exit 2
}

# Drops the own-package version line from a Cargo.toml read on stdin.
strip_toml_version() {
  awk '
    /^\[/ { in_pkg = ($0 == "[package]") }
    in_pkg && !done && /^version[ \t]*=/ { done = 1; next }
    { print }'
}

# Drops the version line of every lock package without a source line.
strip_lock_version() {
  awk '
    function flush(   i, has_src) {
      if (n == 0) return
      has_src = 0
      for (i = 1; i <= n; i++) if (buf[i] ~ /^source[ \t]*=/) has_src = 1
      for (i = 1; i <= n; i++) {
        if (!has_src && buf[i] ~ /^version[ \t]*=/) continue
        print buf[i]
      }
      n = 0
    }
    /^\[\[package\]\]/ { flush() }
    { buf[++n] = $0 }
    END { flush() }'
}

tmp=$(mktemp) || exit 2
trap 'rm -f "$tmp"' EXIT

{
  git ls-tree -r "$rev" -- src crates build.rs | grep -Ev '^[^	]*	crates/[^/]+/Cargo\.toml$'
  echo "--- cargo"
  # Fixed order: root first, then member manifests sorted by path.
  manifests=$(git ls-tree -r --name-only "$rev" -- Cargo.toml 'crates/*/Cargo.toml' | sort)
  for f in $manifests; do
    echo "== $f"
    git show "$rev:$f" | strip_toml_version
  done
  echo "== Cargo.lock"
  git show "$rev:Cargo.lock" | strip_lock_version
} >"$tmp" 2>/dev/null
# A pipeline's failures are hidden by the group; require the inputs directly.
git cat-file -e "$rev:Cargo.toml" 2>/dev/null && git cat-file -e "$rev:Cargo.lock" 2>/dev/null || {
  echo "scanner-key: $rev has no Cargo.toml or Cargo.lock" >&2
  exit 2
}
[ -s "$tmp" ] || { echo "scanner-key: nothing to hash for $rev" >&2; exit 2; }

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum <"$tmp" | cut -d' ' -f1
else
  shasum -a 256 <"$tmp" | cut -d' ' -f1
fi
