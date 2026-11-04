#!/usr/bin/env bash
# Test for scripts/release-check-inherit.sh (release-check-inherit.yml), with
# `gh` stubbed: statuses are served from, and posted to, fixture files.
# Commits on one branch: A (src), B (docs), C (docs), D (src), E (docs).
# Run from the repo root; wired into ci.yml "release-artifact-steps".
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
script="$root/scripts/release-check-inherit.sh"
fail=0
ok()  { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/st" "$work/repo"

# `gh api` stub. GET repos/R/commits/SHA/statuses prints the fixture (newest
# first); POST repos/R/statuses/SHA prepends a status and records the call.
cat >"$work/gh" <<'STUB'
#!/usr/bin/env bash
[ "$1" = api ] || exit 2
shift
method=GET; target=""; state=""; context=""; desc=""
while [ $# -gt 0 ]; do
  case "$1" in
    -X) method=$2; shift 2 ;;
    -f) case "$2" in
          state=*) state=${2#state=} ;;
          context=*) context=${2#context=} ;;
          description=*) desc=${2#description=} ;;
        esac; shift 2 ;;
    --paginate) shift ;;
    *) target=$1; shift ;;
  esac
done
if [ "$method" = POST ]; then
  sha=${target##*/}
  f="$ST/$sha.json"; [ -f "$f" ] || echo '[]' >"$f"
  jq --arg s "$state" --arg c "$context" --arg d "$desc" '[{state:$s,context:$c,description:$d}] + .' "$f" >"$f.new" && mv "$f.new" "$f"
  echo "$sha $context $state $desc" >>"$POSTS"
else
  sha=${target#*/commits/}; sha=${sha%/statuses}
  cat "$ST/$sha.json" 2>/dev/null || echo '[]'
fi
STUB
chmod +x "$work/gh"
export GH="$work/gh" ST="$work/st" POSTS="$work/posts" GITHUB_REPOSITORY=o/r
: >"$POSTS"

cd "$work/repo" || exit 1
git init -q . && git config user.email t@t && git config user.name t && git config commit.gpgsign false
mkdir -p src docs
printf '[package]\nname = "x"\nversion = "0.1.0"\n' >Cargo.toml
printf '[[package]]\nname = "x"\nversion = "0.1.0"\n' >Cargo.lock
mk() { # name, file, content
  echo "$3" >>"$2" && git add -A && git commit -qm "$1" && git rev-parse HEAD
}
A=$(mk A src/main.rs a)
B=$(mk B docs/a.md b)
C=$(mk C docs/a.md c)
D=$(mk D src/main.rs d)
E=$(mk E docs/a.md e)
export INHERIT_REF=HEAD
mkdir -p "$work/s"; cp "$script" "$work/s/"; cp "$root/scripts/scanner-key.sh" "$work/s/"

st_of() { jq -r '.[0].state // "none"' "$ST/$1.json" 2>/dev/null || echo none; }
direct() { # sha state desc
  f="$ST/$1.json"; [ -f "$f" ] || echo '[]' >"$f"
  jq --arg s "$2" --arg d "$3" '[{state:$s,context:"release-check",description:$d}] + .' "$f" >"$f.n" && mv "$f.n" "$f"
}
run() { bash "$work/s/release-check-inherit.sh" >/dev/null 2>&1 || bad "script exited non-zero"; }
posts() { wc -l <"$POSTS" | tr -d ' '; }

# 1. A direct pending: B and C inherit it; D and E have no same-key source.
direct "$A" pending "Queued for main's check"
run
[ "$(st_of "$B")" = pending ] && [ "$(st_of "$C")" = pending ] && ok "B and C inherit pending" || bad "B and C inherit pending"
[ "$(st_of "$D")" = none ] && [ "$(st_of "$E")" = none ] && ok "D and E get nothing" || bad "D and E get nothing"
jq -r '.[0].description' "$ST/$B.json" | grep -q "^Same scanner as ${A:0:8}: Queued" && ok "description names the source" || bad "description names the source"
[ "$(jq -r '.[0].context' "$ST/$B.json")" = release-check ] && ok "context is release-check" || bad "context is release-check"
[ "$(posts)" = 2 ] && ok "2 posts" || bad "2 posts (got $(posts))"

# 2. A turns success: B and C follow; D and E still nothing.
direct "$A" success "PASS: fixed corpus"
run
[ "$(st_of "$B")" = success ] && [ "$(st_of "$C")" = success ] && ok "B and C follow A to success" || bad "B and C follow A to success"
[ "$(st_of "$D")" = none ] && [ "$(st_of "$E")" = none ] && ok "D and E still nothing" || bad "D and E still nothing"

# 3. D gets a direct failure: E inherits; A to C unchanged.
direct "$D" failure "FAIL: PR accuracy A1 3"
run
[ "$(st_of "$E")" = failure ] && ok "E inherits D's failure" || bad "E inherits D's failure"
[ "$(st_of "$A")" = success ] && [ "$(st_of "$B")" = success ] && [ "$(st_of "$C")" = success ] && ok "A to C unchanged" || bad "A to C unchanged"
[ "$(st_of "$D")" = failure ] && ok "D keeps its direct status" || bad "D keeps its direct status"

# 4. No change: a second run posts nothing.
n=$(posts); run
[ "$(posts)" = "$n" ] && ok "re-run posts nothing" || bad "re-run posts nothing"

# 5. Descriptions are cut to 140 characters.
long=$(printf 'x%.0s' $(seq 1 200))
direct "$D" failure "$long"
run
[ "$(jq -r '.[0].description|length' "$ST/$E.json")" -le 140 ] && ok "description cut to 140" || bad "description cut to 140"

exit $fail
