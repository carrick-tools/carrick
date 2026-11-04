#!/usr/bin/env bash
# Tests for scripts/main-check-trigger.sh (carrick-cloud#2916, sub-ticket A).
# A throwaway repo with a src commit, a docs commit and a version-only commit:
# docs and version-only skip, src dispatches, an unknown or all-zero `before`
# dispatches. Run from anywhere; wired into ci.yml (release-artifact-steps).
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
trigger="$root/scripts/main-check-trigger.sh"
fail=0
ok()  { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work" || exit 1
git init -q . && git config user.email t@t && git config user.name t && git config commit.gpgsign false
mkdir -p src crates/m/src docs
echo 'fn main() {}' >src/main.rs
echo '// lib' >crates/m/src/lib.rs
printf '[package]\nname = "x"\nversion = "1.0.0"\n' >Cargo.toml
printf '[package]\nname = "m"\nversion = "1.0.0"\n' >crates/m/Cargo.toml
printf '[[package]]\nname = "x"\nversion = "1.0.0"\n' >Cargo.lock
echo 'fn b() {}' >build.rs
echo docs >docs/a.md
git add -A && git commit -qm base
base=$(git rev-parse HEAD)

echo 'more docs' >>docs/a.md && git add -A && git commit -qm docs
docs=$(git rev-parse HEAD)
sed -i.bak 's/1\.0\.0/1.0.1/' Cargo.toml Cargo.lock crates/m/Cargo.toml && rm -f ./*.bak crates/m/*.bak
git add -A && git commit -qm version
ver=$(git rev-parse HEAD)
echo 'fn changed() {}' >>src/main.rs && git add -A && git commit -qm src
src=$(git rev-parse HEAD)

# check <name> <before> <after> <want word>
check() {
  local out
  out=$("$trigger" "$2" "$3" 2>&1)
  if [[ "$out" =~ ^"$4"\ [0-9a-f]{64}$ ]]; then ok "$1: $4"; else bad "$1: want '$4 <key>', got '$out'"; fi
}
check "docs commit" "$base" "$docs" skip
check "version-only commit" "$docs" "$ver" skip
check "src commit" "$ver" "$src" dispatch
check "docs and version span" "$base" "$ver" skip
check "all-zero before" 0000000000000000000000000000000000000000 "$src" dispatch
check "unknown before" deadbeefdeadbeefdeadbeefdeadbeefdeadbeef "$src" dispatch
# The key printed is the after commit's.
want=$("$root/scripts/scanner-key.sh" "$src")
[ "$("$trigger" "$ver" "$src" | cut -d' ' -f2)" = "$want" ] && ok "prints the after key" || bad "after key differs"
# An unreadable after fails loudly.
if "$trigger" "$base" nosuchrev >/dev/null 2>&1; then bad "unknown after exits 0"; else ok "unknown after fails"; fi

[ "$fail" -eq 0 ] && echo "all passed" || { echo "failed"; exit 1; }
