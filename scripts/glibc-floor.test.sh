#!/usr/bin/env bash
# scripts/glibc-floor.sh against stubbed `objdump -T` output: it passes a
# binary at or below the floor, and fails one above it, one whose versions only
# sort above the floor numerically (2.3 < 2.28 < 2.100), and one that
# references no glibc at all. Run by CI (ci.yml, "Release artifact steps");
# the release is the only other place the check runs.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/glibc-floor.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() { echo "FAIL: $*"; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# `objdump -T <binary>` prints the file named by the binary's own contents:
# each fake binary holds the symbol lines its objdump would print.
cat > "$work/objdump" <<'STUB'
#!/usr/bin/env bash
[ "$1" = "-T" ] || exit 64
cat "$2"
STUB
chmod +x "$work/objdump"
export OBJDUMP="$work/objdump"

symbols() {
  local out="$1"
  shift
  : > "$out"
  for version in "$@"; do
    printf '0000000000000000      DF *UND*\t0000000000000000 (GLIBC_%s) sym_%s\n' "$version" "$version" >> "$out"
  done
}

symbols "$work/at-floor" 2.2.5 2.3 2.17 2.28
symbols "$work/below-floor" 2.2.5 2.3.4 2.17
symbols "$work/above-floor" 2.2.5 2.28 2.39
symbols "$work/numeric-order" 2.3 2.9 2.100
printf '0000000000000000      DF *UND*\t0000000000000000 (GCC_3.0) _Unwind_Resume\n' > "$work/no-glibc"

expect() {
  local want="$1" label="$2"
  shift 2
  local code=0
  bash "$script" "$@" > "$work/out" 2>&1 || code=$?
  if [ "$want" = "pass" ] && [ "$code" = "0" ]; then
    pass "$label"
  elif [ "$want" = "fail" ] && [ "$code" != "0" ]; then
    pass "$label"
  else
    fail "$label (exit $code)"
  fi
  sed 's/^/    /' "$work/out"
}

expect pass "a binary at the floor passes" "$work/at-floor"
expect pass "a binary below the floor passes" "$work/below-floor"
expect fail "a binary needing GLIBC_2.39 fails" "$work/above-floor"
expect fail "versions compare numerically, so GLIBC_2.100 is above 2.28" "$work/numeric-order"
expect fail "a binary with no GLIBC_ version fails" "$work/no-glibc"
expect fail "one binary above the floor fails the whole check" "$work/at-floor" "$work/above-floor"
expect fail "a binary objdump cannot read fails" "$work/missing"
GLIBC_FLOOR=2.39 expect pass "the floor is read from GLIBC_FLOOR" "$work/above-floor"

if grep -q "needs GLIBC_2.39" <(OBJDUMP="$OBJDUMP" bash "$script" "$work/above-floor" 2>&1 || true); then
  pass "the failure names the version found"
else
  fail "the failure does not name GLIBC_2.39"
fi

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all glibc floor checks passed"
