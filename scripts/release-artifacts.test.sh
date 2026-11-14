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
# GH_FAIL_LISTER=1: the lister's upload fails, as the network or an existing asset can.
if [ "${GH_FAIL_LISTER:-}" = "1" ] && [ "$1" = "release" ] && [[ " $* " == *carrick-lister* ]]; then
  echo "upload failed" >&2
  exit 1
fi
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

# A release step sequence, as the workflow runs it, for one lister state. The
# attach step continues on error; its outcome goes to the dispatch.
release() {
  : > "$calls"
  "$script" upload-matcher carrick-v0.0.0 > /dev/null
  local attached=success
  LISTER_OK="$1" "$script" upload-lister carrick-v0.0.0 > /dev/null 2>&1 || attached=failure
  LISTER_OK="$1" LISTER_ATTACHED="$attached" "$script" dispatch carrick-v0.0.0 > /dev/null
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

# The lister passed but its upload failed: the matcher is still announced,
# and the cloud is not told to fetch a lister that is not attached.
GH_FAIL_LISTER=1 release "$(status_of success true success | cut -d' ' -f1)"
expect_call "$matcher_upload" "lister upload failed: matcher attached"
expect_call "client_payload[wasm_sha256]=" "lister upload failed: matcher announced"
refuse_call "lister_manifest_sha256" "lister upload failed: lister not announced"

# Told it passed, but its files are missing: nothing of it ships.
rm lister-artifact/carrick-lister.mjs
release true
refuse_call "carrick-lister" "missing lister files: not attached"
refuse_call "lister_manifest_sha256" "missing lister files: not announced"

# The type reasons list (carrick#4231) rides with the matcher: a tag that has
# it ships it byte for byte, in the checksums, attached and announced by its
# sha256; a tag from before it ships the matcher alone, as before.
reasons_tag="$work/reasons-tag"
mkdir -p "$reasons_tag/wasm-artifact" "$reasons_tag/src/sidecar/src/capture"
for f in carrick_match.js carrick_match.d.ts carrick_match_bg.wasm; do echo "$f" > "$reasons_tag/wasm-artifact/$f"; done
printf '{\n  "declared": { "sentence": "declared that way", "decided": "never" }\n}\n' \
  > "$reasons_tag/src/sidecar/src/capture/type-reasons.json"
reasons_sha="$(sha256sum "$reasons_tag/src/sidecar/src/capture/type-reasons.json" | awk '{print $1}')"
(
  cd "$reasons_tag"
  : > "$calls"
  "$script" checksums > /dev/null 2>&1 || true
  "$script" upload-matcher carrick-v0.0.0 > /dev/null || true
  "$script" dispatch carrick-v0.0.0 > /dev/null || true
)
if cmp -s "$reasons_tag/src/sidecar/src/capture/type-reasons.json" "$reasons_tag/wasm-artifact/type-reasons.json"; then
  pass "reasons list: copied byte for byte beside the matcher"
else
  fail "reasons list: not copied byte for byte into wasm-artifact/"
fi
if grep -qx "$reasons_sha  type-reasons.json" "$reasons_tag/wasm-artifact/carrick_match.sha256" \
  && [ "$(wc -l < "$reasons_tag/wasm-artifact/carrick_match.sha256" | tr -d ' ')" = 4 ]; then
  pass "reasons list: in carrick_match.sha256 with the three matcher files"
else
  fail "reasons list: checksums are: $(cat "$reasons_tag/wasm-artifact/carrick_match.sha256" 2>/dev/null)"
fi
expect_call "$matcher_upload wasm-artifact/type-reasons.json --clobber" "reasons list: attached with the matcher"
expect_call "client_payload[type_reasons_sha256]=$reasons_sha" "reasons list: announced by its sha256"
expect_call "client_payload[wasm_sha256]=" "reasons list: the matcher is still announced"

# A tag from before the list: the checksums, upload and dispatch are the matcher's alone.
old_tag="$work/old-tag"
mkdir -p "$old_tag/wasm-artifact"
for f in carrick_match.js carrick_match.d.ts carrick_match_bg.wasm; do echo "$f" > "$old_tag/wasm-artifact/$f"; done
(
  cd "$old_tag"
  : > "$calls"
  "$script" checksums > "$work/old-log" 2>&1 || true
  "$script" upload-matcher carrick-v0.0.0 > /dev/null || true
  "$script" dispatch carrick-v0.0.0 > /dev/null || true
)
if [ "$(wc -l < "$old_tag/wasm-artifact/carrick_match.sha256" | tr -d ' ')" = 3 ] && grep -q "type-reasons.json" "$work/old-log"; then
  pass "no reasons list: three checksums, and the log says the list is absent"
else
  fail "no reasons list: checksums $(cat "$old_tag/wasm-artifact/carrick_match.sha256" 2>/dev/null); log $(cat "$work/old-log")"
fi
expect_call "$matcher_upload --clobber" "no reasons list: the matcher attached alone"
refuse_call "type-reasons" "no reasons list: nothing of it attached"
refuse_call "type_reasons_sha256" "no reasons list: nothing of it announced"
expect_call "client_payload[wasm_sha256]=" "no reasons list: the matcher announced"

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
checksums = step("Write checksums")
matcher = step("Attach the matcher to the release")
build = step("Build the surface lister")
test = step("Test the surface lister on Node 22")
decide = step("Decide whether the surface lister ships")
attach = step("Attach the surface lister to the release")
dispatch = step("Dispatch pin bump to carrick-cloud")
failed = step("Surface lister failed")
if not problems:
    if not checksums[0] < matcher[0]:
        problems.append("the checksums are not written before the matcher is attached")
    if "release-artifacts.sh checksums" not in checksums[1] or re.search(r"^\s+if:", checksums[1], re.M):
        problems.append("the checksums step does not run the checksums arm unconditionally")
    if not matcher[0] < build[0] < test[0] < decide[0] < attach[0] < dispatch[0] < failed[0]:
        problems.append("steps out of order: matcher upload, lister build, test, decide, lister upload, dispatch, lister failure")
    for name, (_, body) in (("build", build), ("test", test), ("upload", attach)):
        if "continue-on-error: true" not in body:
            problems.append(f"the lister {name} step can stop the job")
    for name, (_, body) in (("matcher upload", matcher), ("decision", decide), ("lister upload", attach), ("dispatch", dispatch)):
        if re.search(r"^\s+if:", body, re.M):
            problems.append(f"the {name} step is conditional")
    if "LISTER_ATTACHED: ${{ steps.lister_attach.outcome }}" not in dispatch[1] or "id: lister_attach" not in attach[1]:
        problems.append("the dispatch does not read whether the lister was attached")
    if "if: always() && (steps.lister.outputs.status == 'failed' || steps.lister_attach.outcome == 'failure')" not in failed[1]:
        problems.append("the lister failure step does not report every failure")
    if failed[0] != len(steps) - 1:
        problems.append("the lister failure step is not the last")
for p in problems:
    print(p)
sys.exit(1 if problems else 0)
PY
then pass "workflow: matcher first, lister steps (attach too) continue on error, failure reported last"
else fail "workflow step order or guards"
fi

# The cli announcement: event, tag, version and source sha.
: > "$calls"
"$script" announce-cli carrick-v0.0.0 > /dev/null
expect_call "event_type=carrick-cli-published" "announce-cli: sends the event"
expect_call "client_payload[tag]=carrick-v0.0.0" "announce-cli: names the tag"
expect_call "client_payload[version]=0.0.0" "announce-cli: names the version"
expect_call "client_payload[source_sha]=abc123" "announce-cli: names the source sha"

# Main's check dispatch: event, sha and key, nothing else.
: > "$calls"
"$script" main-check 0123456789abcdef0123456789abcdef01234567 abc123key > /dev/null
expect_call "api repos/carrick-tools/carrick-cloud/dispatches" "main-check: dispatches to carrick-cloud"
expect_call "event_type=carrick-main-check" "main-check: sends the event"
expect_call "client_payload[sha]=0123456789abcdef0123456789abcdef01234567" "main-check: names the sha"
expect_call "client_payload[key]=abc123key" "main-check: names the key"
if "$script" main-check onlysha > /dev/null 2>&1; then fail "main-check: a missing key exits 0"; else pass "main-check: a missing key fails"; fi

# A failed dispatch fails the step and names the backfill command.
printf '#!/usr/bin/env bash\nexit 1\n' > "$work/gh-fail"
chmod +x "$work/gh-fail"
if out="$(GH="$work/gh-fail" "$script" announce-cli carrick-v0.0.0 2>&1)"; then
  fail "announce-cli: a failed dispatch exits 0"
elif [[ "$out" == *"::error::"*"gh workflow run scanner-pin-bump.yml -R carrick-tools/carrick-cloud -f tag=carrick-v0.0.0"* ]]; then
  pass "announce-cli: a failed dispatch is an error naming the backfill"
else
  fail "announce-cli: failed dispatch message: $out"
fi

# The release workflow runs it only after publish, and only for the latest.
if python3 - "$here/../.github/workflows/release.yml" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
m = re.search(r"\n  announce-cli:\n(.*?)(?=\n  [a-z][a-z-]*:\n|\Z)", text, re.S)
problems = []
if not m:
    problems.append("no announce-cli job")
else:
    body = m.group(1)
    needs = re.search(r"needs: \[(.*?)\]", body)
    if not needs or "publish" not in [n.strip() for n in needs.group(1).split(",")]:
        problems.append("announce-cli does not need publish")
    if "needs.target.outputs.is_latest == 'true'" not in body:
        problems.append("announce-cli is not gated on is_latest")
    if "announce-cli" not in body.split("steps:")[1]:
        problems.append("announce-cli job does not run the announce-cli arm")
for p in problems:
    print(p)
sys.exit(1 if problems else 0)
PY
then pass "workflow: announce-cli needs publish and is gated on is_latest"
else fail "release.yml announce-cli job"
fi

echo
if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "all release step checks passed"
