#!/usr/bin/env bash
# Test for scripts/release-cut.sh (release.yml `cut` job), with `gh` stubbed:
# tags, Releases and PR labels live in fixture files, and every write is
# recorded in order. Commits on main: A, R (the version PR's merge, 0.2.0), S.
# Run from the repo root; wired into ci.yml "release-artifact-steps".
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
script="$root/scripts/release-cut.sh"
fail=0
ok()  { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/repo"

# `gh` stub. State: $W/tags/<tag> holds a sha, $W/releases/<tag> holds notes,
# $W/labels holds one label per line. Writes are appended to $W/calls.
# FAIL_RELEASE=1 makes `gh release create` fail.
cat >"$work/gh" <<'STUB'
#!/usr/bin/env bash
if [ "$1" = release ]; then
  tag=$3
  if [ "$2" = view ]; then [ -f "$W/releases/$tag" ]; exit; fi
  if [ "$2" = create ]; then
    [ "${FAIL_RELEASE:-0}" = 1 ] && { echo "HTTP 502" >&2; exit 1; }
    notes=""; latest=""
    while [ $# -gt 0 ]; do
      case "$1" in --notes-file) notes=$2; shift ;; --latest=*) latest=$1 ;; esac; shift
    done
    mkdir -p "$W/releases"; cp "$notes" "$W/releases/$tag"
    echo "release-create $tag $latest" >>"$W/calls"; exit 0
  fi
  exit 2
fi
[ "$1" = api ] || exit 2
shift
method=GET; target=""; fields=()
while [ $# -gt 0 ]; do
  case "$1" in
    -X) method=$2; shift 2 ;;
    -f) fields+=("$2"); shift 2 ;;
    --jq) shift 2 ;;
    *) target=$1; shift ;;
  esac
done
case "$method $target" in
  "GET "*/git/ref/tags/*)
    tag=${target##*/}; [ -f "$W/tags/$tag" ] || { echo "Not Found" >&2; exit 1; }
    cat "$W/tags/$tag" ;;
  "POST "*/git/refs)
    ref=${fields[0]#ref=refs/tags/}; sha=${fields[1]#sha=}
    mkdir -p "$W/tags"; echo "$sha" >"$W/tags/$ref"
    echo "tag $ref $sha" >>"$W/calls" ;;
  "GET "*/labels) cat "$W/labels" ;;
  "POST "*/labels)
    echo "${fields[0]#labels[]=}" >>"$W/labels"
    echo "label-add ${fields[0]#labels[]=}" >>"$W/calls" ;;
  "DELETE "*/labels/*)
    grep -vxF 'autorelease: pending' "$W/labels" >"$W/labels.new"; mv "$W/labels.new" "$W/labels"
    echo "label-remove ${target##*/}" >>"$W/calls" ;;
  *) echo "stub: unexpected $method $target" >&2; exit 2 ;;
esac
STUB
chmod +x "$work/gh"
export GH="$work/gh" W="$work/state" GITHUB_REPOSITORY=o/r

cd "$work/repo" || exit 1
git init -q -b main
git config user.email t@t; git config user.name t
commit() { printf '%s' "$2" >CHANGELOG.md; git add CHANGELOG.md; git commit -q --allow-empty -m "$1"; git rev-parse HEAD; }
log='# Changelog

## [0.2.0](https://x/compare/carrick-v0.1.0...carrick-v0.2.0) (2026-11-12)

### Features

* new thing ([#2](https://x/2))

## [0.1.0](https://x) (2026-11-01)

* old thing
'
A=$(commit "feat: new thing (#2)" "# Changelog
")
R=$(commit "chore(main): release carrick 0.2.0 (#3)" "$log")
S=$(commit "fix: after the bump (#4)" "$log")

reset() {
  rm -rf "$W"; mkdir -p "$W/tags" "$W/releases"; : >"$W/calls"
  printf 'autorelease: pending\n' >"$W/labels"
}
run() { bash "$script" "$@" >"$work/out" 2>&1; }

# 1. A fresh cut: three writes in order, the pending label removed last.
reset
if run "$S" 0.2.0 3 "$R"; then ok "fresh cut exits 0"; else bad "fresh cut exits $? : $(cat "$work/out")"; fi
want="tag carrick-v0.2.0 $S
release-create carrick-v0.2.0 --latest=false
label-add autorelease: tagged
label-remove autorelease:%20pending"
[ "$(cat "$W/calls")" = "$want" ] && ok "fresh cut writes tag, Release, labels, in that order" \
  || bad "fresh cut calls: $(cat "$W/calls")"
notes="$W/releases/carrick-v0.2.0"
head -1 "$notes" | grep -q '^## \[0.2.0\]' && ok "notes open with the version's CHANGELOG section" || bad "notes: $(head -3 "$notes")"
grep -q 'old thing' "$notes" && bad "notes carry the previous version's section" || ok "notes stop at the next version"
grep -q "^### Also in this release .*${S:0:8})" "$notes" && grep -qF "* fix: after the bump (#4) (${S:0:7})" "$notes" \
  && ok "notes list the commit merged after the version PR" || bad "also-in: $(cat "$notes")"

# 2. Run again: everything is done, nothing is written.
: >"$W/calls"
if run "$S" 0.2.0 3 "$R" && [ ! -s "$W/calls" ]; then ok "a second run writes nothing"; else bad "second run: $(cat "$W/calls") $(cat "$work/out")"; fi

# 3. Resume after the tag: the tag is skipped, the rest runs.
reset
echo "$S" >"$W/tags/carrick-v0.2.0"
run "$S" 0.2.0 3 "$R"
grep -q '^tag ' "$W/calls" && bad "resume re-tagged" || ok "resume after the tag skips the tag"
[ "$(wc -l <"$W/calls" | tr -d ' ')" = 3 ] && ok "resume writes the Release and both labels" || bad "resume calls: $(cat "$W/calls")"

# 4. The Release step fails: the PR keeps `autorelease: pending`, exit non-zero.
reset
if FAIL_RELEASE=1 run "$S" 0.2.0 3 "$R"; then bad "failed Release step exits 0"; else ok "failed Release step exits non-zero"; fi
grep -q '^label' "$W/calls" && bad "failed Release step still relabelled" || ok "failed Release step leaves no relabel"
grep -qxF 'autorelease: pending' "$W/labels" && ok "PR stays pending, so the next run resumes" || bad "labels: $(cat "$W/labels")"

# 5. The tag exists at another sha: stop before any write.
reset
echo "$A" >"$W/tags/carrick-v0.2.0"
if run "$S" 0.2.0 3 "$R"; then bad "tag elsewhere exits 0"; else ok "tag at another sha exits non-zero"; fi
[ ! -s "$W/calls" ] && ok "tag at another sha writes nothing" || bad "tag elsewhere calls: $(cat "$W/calls")"

# 6. The cut sha is the version PR's merge commit: no "Also in this release".
reset
run "$R" 0.2.0 3 "$R" || bad "cut at the version commit failed: $(cat "$work/out")"
[ -s "$W/releases/carrick-v0.2.0" ] || bad "cut at the version commit made no Release"
grep -q 'Also in this release' "$W/releases/carrick-v0.2.0" && bad "also-in heading on an empty list" || ok "cut at the version commit has no also-in heading"

# 7. No CHANGELOG section for the version: no Release, no relabel.
reset
if run "$S" 0.3.0 3 "$R"; then bad "missing section exits 0"; else ok "missing CHANGELOG section exits non-zero"; fi
grep -q -e '^release' -e '^label' "$W/calls" && bad "missing section wrote: $(cat "$W/calls")" || ok "missing section leaves no Release and no relabel"

# 8. Bad arguments are refused before any call.
reset
for args in "${S:0:8} 0.2.0 3 $R" "$S v0.2.0 3 $R" "$S 0.2.0 x $R" "$S 0.2.0 3"; do
  # shellcheck disable=SC2086
  if run $args; then bad "accepted: $args"; fi
done
[ ! -s "$W/calls" ] && ok "bad arguments are refused with no call" || bad "bad args calls: $(cat "$W/calls")"

exit $fail
