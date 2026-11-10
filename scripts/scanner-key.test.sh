#!/usr/bin/env bash
# Tests for scripts/scanner-key.sh and scripts/release-diff-check.py.
# Builds a throwaway repo from this checkout's release files (so it follows the
# real layout) and checks the four cases the release automation relies on.
# Run from the repo root; wired into the lint job in .github/workflows/ci.yml.
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
key="$root/scripts/scanner-key.sh"
chk="$root/scripts/release-diff-check.py"
fail=0
ok()  { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work" || exit 1
git init -q . && git config user.email t@t && git config user.name t && git config commit.gpgsign false

# Release files from the checkout, plus a stub for the scanner source.
files="release-please-config.json .release-please-manifest.json CHANGELOG.md Cargo.toml Cargo.lock build.rs crates/carrick-match/Cargo.toml npm/carrick/package.json npm/carrick/package-lock.json plugin/vscode/package.json plugin/vscode/package-lock.json plugin/.claude-plugin/plugin.json .claude-plugin/marketplace.json"
for f in $files; do
  mkdir -p "$(dirname "$f")"
  git -C "$root" show "HEAD:$f" >"$f" || { echo "FAIL cannot read $f from HEAD"; exit 1; }
done
mkdir -p src crates/carrick-match/src docs
echo 'fn main() {}' >src/main.rs
echo '// lib' >crates/carrick-match/src/lib.rs
echo docs >docs/a.md
git add -A && git commit -qm base
base=$(git rev-parse HEAD)
old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
new="${old%.*}.$(( ${old##*.} + 1 ))"

# 1. A release commit: version values move, nothing else.
sed -i.bak "s/\"$old\"/\"$new\"/g" Cargo.toml Cargo.lock crates/carrick-match/Cargo.toml \
  npm/carrick/package.json plugin/vscode/package.json \
  plugin/vscode/package-lock.json plugin/.claude-plugin/plugin.json .claude-plugin/marketplace.json .release-please-manifest.json
# The npm lockfile's platform entries carry hashes release-please cannot move
# (#3676): bump only the lines above them (own version, root optionalDependencies).
sed -i.bak "1,/\"node_modules\/@carrick-tools/ s/\"$old\"/\"$new\"/g" npm/carrick/package-lock.json
find . -name '*.bak' -not -path './.git/*' -delete
printf '\n## %s\n- a change\n' "$new" >>CHANGELOG.md
git add -A && git commit -qm release
release=$(git rev-parse HEAD)
[ "$("$key" "$base")" = "$("$key" "$release")" ] && ok "release diff: same key" || bad "release diff: same key"
python3 "$chk" "$base" "$release" >/dev/null 2>&1; [ $? -eq 0 ] && ok "release diff: exit 0" || bad "release diff: exit 0"

# 2. A docs-only commit: same key (the check is for release diffs, so exit 1).
echo more >>docs/a.md && git add -A && git commit -qm docs
docs=$(git rev-parse HEAD)
[ "$("$key" "$release")" = "$("$key" "$docs")" ] && ok "docs-only: same key" || bad "docs-only: same key"
python3 "$chk" "$release" "$docs" >/dev/null 2>&1; [ $? -eq 1 ] && ok "docs-only: not a release diff (exit 1)" || bad "docs-only: exit 1"

# 3. A scanner source change: different key, exit 1 naming the path.
echo '// changed' >>src/main.rs && git add -A && git commit -qm src
src=$(git rev-parse HEAD)
[ "$("$key" "$docs")" != "$("$key" "$src")" ] && ok "src change: different key" || bad "src change: different key"
out=$(python3 "$chk" "$docs" "$src" 2>&1); rc=$?
[ $rc -eq 1 ] && echo "$out" | grep -q '^src/main.rs' && ok "src change: exit 1 names src/main.rs" || bad "src change: exit 1 names src/main.rs"

# 4. A src change hidden inside a release diff: exit 1 and different key.
git checkout -q "$base"
sed -i.bak "s/\"$old\"/\"$new\"/g" Cargo.toml Cargo.lock .release-please-manifest.json && rm -f ./*.bak
echo '// sneaked in' >>crates/carrick-match/src/lib.rs
git add -A && git commit -qm "release plus src"
mixed=$(git rev-parse HEAD)
out=$(python3 "$chk" "$base" "$mixed" 2>&1); rc=$?
[ $rc -eq 1 ] && echo "$out" | grep -q 'crates/carrick-match/src/lib.rs' && ok "release plus src: exit 1" || bad "release plus src: exit 1"
[ "$("$key" "$base")" != "$("$key" "$mixed")" ] && ok "release plus src: different key" || bad "release plus src: different key"

# 5. More than a version moves in a release file: exit 1.
git checkout -q "$base"
sed -i.bak 's/^edition = .*/edition = "2021"/' Cargo.toml && rm -f ./*.bak
git add -A && git commit -qm "edition"
out=$(python3 "$chk" "$base" "$(git rev-parse HEAD)" 2>&1); rc=$?
[ $rc -eq 1 ] && echo "$out" | grep -q '^Cargo.toml' && ok "non-version Cargo.toml change: exit 1" || bad "non-version Cargo.toml change: exit 1"
[ "$("$key" "$base")" != "$("$key" HEAD)" ] && ok "non-version Cargo.toml change: different key" || bad "non-version Cargo.toml change: different key"

# 6. A JSON file changes outside the configured jsonpaths: exit 1.
git checkout -q "$base"
sed -i.bak 's/"name": "[^"]*"/"name": "renamed"/' plugin/vscode/package.json && rm -f ./*.bak
git add -A && git commit -qm "json"
python3 "$chk" "$base" "$(git rev-parse HEAD)" >/dev/null 2>&1; [ $? -eq 1 ] && ok "json change outside jsonpaths: exit 1" || bad "json change outside jsonpaths: exit 1"

# 7. Could not check: exit 2.
python3 "$chk" "$base" no-such-rev >/dev/null 2>&1; [ $? -eq 2 ] && ok "unknown rev: exit 2" || bad "unknown rev: exit 2"
"$key" no-such-rev >/dev/null 2>&1; [ $? -eq 2 ] && ok "key of unknown rev: exit 2" || bad "key of unknown rev: exit 2"

[ $fail -eq 0 ] && echo "all passed" || echo "FAILED"
exit $fail
