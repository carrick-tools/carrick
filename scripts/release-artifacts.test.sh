#!/usr/bin/env bash
# Dry run of the release steps in .github/workflows/wasm-artifact.yml
# (scripts/release-artifacts.sh), with `gh` stubbed: a release whose surface
# lister fails to build or test still attaches and announces the matcher, and
# attaches and announces no lister. Run by CI (ci.yml, "Release artifact
# steps"); a tag is the only other place these steps ever run.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/release-artifacts.sh"
workflow="$here/../.github/workflows/wasm-artifact.yml"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
fail() { echo "FAIL: $*"; failures=$((failures + 1)); }
pass() { echo "ok: $*"; }

# A release workspace: the matcher's files and a built lister.
cd "$work"
mkdir -p wasm-artifact lister-artifact
for f in carrick_match.js carrick_match.d.ts carrick_match_bg.wasm; do echo "$f" > "wasm-artifact/$f"; done
(cd wasm-artifact && sha256sum carrick_match.js carrick_match.d.ts carrick_match_bg.wasm > carrick_match.sha256)
echo "bundle" > lister-artifact/carrick-lister.mjs
echo '{"tag":"carrick-v0.0.0"}' > lister-artifact/carrick-lister.manifest.json
manifest_sha="$(sha256sum lister-artifact/carrick-lister.manifest.json | awk '{print $1}')"

# `gh`, recording each call's arguments on one line.
calls="$work/calls"
cat > "$work/gh" <<'STUB'
#!/usr/bin/env bash
printf '%s ' "$@" >> "$CALLS"
printf '\n' >> "$CALLS"
STUB
chmod +x "$work/gh"
export GH="$work/gh" CALLS="$calls" SOURCE_SHA=abc123

# The decision: ok only when the build ran, built, and its tests passed.
status_of() {
  local out="$work/out"
  : > "$out"
  BUILD_OUTCOME="$1" BUILT="$2" TEST_OUTCOME="$3" GITHUB_OUTPUT="$out" "$script" lister-status > "$work/log"
  echo "$(grep '^ok=' "$out" | cut -d= -f2) $(grep '^status=' "$out" | cut -d= -f2)"
}
check_status() {
  local got
  got="$(status_of "$1" "$2" "$3")"
  if [ "$got" = "$4" ]; then pass "lister-status $1/$2/$3 -> $4"; else fail "lister-status $1/$2/$3: got '$got', want '$4'"; fi
}
check_status success true success "true passed"
check_status failure "" skipped "false failed"
check_status success true failure "false failed"
check_status success true cancelled "false failed"
check_status success false skipped "false absent"
status_of failure "" skipped > /dev/null
if grep -q '::warning' "$work/log"; then pass "a failed lister warns"; else fail "a failed lister prints no warning"; fi

# A release step sequence, as the workflow runs it, for one lister state.
release() {
  : > "$calls"
  "$script" upload-matcher carrick-v0.0.0 > /dev/null
  LISTER_OK="$1" "$script" upload-lister carrick-v0.0.0 > /dev/null
  LISTER_OK="$1" "$script" dispatch carrick-v0.0.0 > /dev/null
}
matcher_upload='release upload carrick-v0.0.0 wasm-artifact/carrick_match.js wasm-artifact/carrick_match.d.ts wasm-artifact/carrick_match_bg.wasm wasm-artifact/carrick_match.sha256'
expect_call() { if grep -qF -- "$1" "$calls"; then pass "$2"; else fail "$2: no call with '$1' in: $(cat "$calls")"; fi; }
refuse_call() { if grep -qF -- "$1" "$calls"; then fail "$2: found '$1'"; else pass "$2"; fi; }

# The lister failed: the matcher is attached and announced, the lister is not.
release "$(status_of failure "" skipped | cut -d' ' -f1)"
expect_call "$matcher_upload" "failed lister: matcher attached"
expect_call "client_payload[wasm_sha256]=" "failed lister: matcher announced"
expect_call "client_payload[tag]=carrick-v0.0.0" "failed lister: the dispatch names the tag"
refuse_call "carrick-lister" "failed lister: lister not attached"
refuse_call "lister_manifest_sha256" "failed lister: lister not announced"

# A tag with no lister: the matcher alone.
release "$(status_of success false skipped | cut -d' ' -f1)"
expect_call "$matcher_upload" "no lister: matcher attached"
refuse_call "lister_manifest_sha256" "no lister: lister not announced"

# The lister passed: both are attached and announced, the manifest by its sha.
release "$(status_of success true success | cut -d' ' -f1)"
expect_call "$matcher_upload" "passed lister: matcher attached"
expect_call "release upload carrick-v0.0.0 lister-artifact/carrick-lister.mjs lister-artifact/carrick-lister.manifest.json" "passed lister: lister attached"
expect_call "client_payload[lister_manifest_sha256]=$manifest_sha" "passed lister: manifest sha announced"

# Told it passed, but its files are missing: nothing of it ships.
rm lister-artifact/carrick-lister.mjs
release true
refuse_call "carrick-lister" "missing lister files: not attached"
refuse_call "lister_manifest_sha256" "missing lister files: not announced"

# The workflow runs the steps in that order, and no lister step can stop it.
if python3 - "$workflow" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
steps = re.split(r"\n      - (?=name: |uses: )", text)
by_name = {}
for i, step in enumerate(steps):
    m = re.match(r"name: (.+)", step)
    if m:
        by_name[m.group(1).strip()] = (i, step)
problems = []
def step(name):
    if name not in by_name:
        problems.append(f"no step named {name!r}")
        return (None, "")
    return by_name[name]
matcher = step("Attach the matcher to the release")
build = step("Build the surface lister")
test = step("Test the surface lister on Node 22")
decide = step("Decide whether the surface lister ships")
attach = step("Attach the surface lister to the release")
dispatch = step("Dispatch pin bump to carrick-cloud")
failed = step("Surface lister failed")
if not problems:
    if not matcher[0] < build[0] < test[0] < decide[0] < attach[0] < dispatch[0] < failed[0]:
        problems.append("steps out of order: matcher upload, lister build, test, decide, lister upload, dispatch, lister failure")
    for name, (_, body) in (("build", build), ("test", test)):
        if "continue-on-error: true" not in body:
            problems.append(f"the lister {name} step can stop the job")
    for name, (_, body) in (("matcher upload", matcher), ("decision", decide), ("lister upload", attach), ("dispatch", dispatch)):
        if re.search(r"^\s+if:", body, re.M):
            problems.append(f"the {name} step is conditional")
    if "if: always() && steps.lister.outputs.status == 'failed'" not in failed[1]:
        problems.append("the lister failure step does not report every failure")
    if failed[0] != len(steps) - 1:
        problems.append("the lister failure step is not the last")
for p in problems:
    print(p)
sys.exit(1 if problems else 0)
PY
then pass "workflow: matcher first, lister steps continue on error, failure reported last"
else fail "workflow step order or guards"
fi

echo
if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all release step checks passed"
