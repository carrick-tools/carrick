#!/usr/bin/env bash
# The release steps of .github/workflows/wasm-artifact.yml that decide what a
# release attaches and announces, kept here so CI can run them without a tag
# (scripts/release-artifacts.test.sh).
#
# The matcher (carrick-match wasm) is always attached and always announced.
# The surface lister (carrick#1660) is attached, and its manifest's sha256
# sent, only when it built and passed its tests: a lister failure must never
# stop the matcher, whose pin bump every cloud deploy waits on.
#
#   release-artifacts.sh upload-matcher <tag>  runs `gh release upload` (first, before the lister builds)
#   release-artifacts.sh lister-status         reads BUILD_OUTCOME, BUILT, TEST_OUTCOME;
#                                              writes ok= and status= to $GITHUB_OUTPUT
#   release-artifacts.sh upload-lister <tag>   reads LISTER_OK; uploads nothing unless it passed
#   release-artifacts.sh dispatch <tag>        reads LISTER_OK, LISTER_ATTACHED (the attach step's
#                                              outcome), SOURCE_SHA; runs `gh api`
#
# Paths are relative to the working directory: wasm-artifact/ (matcher) and
# lister-artifact/ (lister). GH names the gh binary (a stub in the test).
set -euo pipefail

GH="${GH:-gh}"
LISTER_ENTRY="lister-artifact/carrick-lister.mjs"
LISTER_MANIFEST="lister-artifact/carrick-lister.manifest.json"

# The lister ships when the caller says it passed and both files are there.
lister_ships() {
  [ "${LISTER_OK:-false}" = "true" ] && [ -f "$LISTER_ENTRY" ] && [ -f "$LISTER_MANIFEST" ]
}

case "${1:-}" in
  lister-status)
    # BUILT is `false` only when the tag has no lister to build.
    if [ "${BUILD_OUTCOME:-}" = "success" ] && [ "${BUILT:-}" = "false" ]; then
      ok=false status=absent
      echo "No surface lister at this tag; the release carries the matcher only."
    elif [ "${BUILD_OUTCOME:-}" = "success" ] && [ "${BUILT:-}" = "true" ] && [ "${TEST_OUTCOME:-}" = "success" ]; then
      ok=true status=passed
    else
      ok=false status=failed
      echo "::warning title=Surface lister not released::build ${BUILD_OUTCOME:-unknown}, tests ${TEST_OUTCOME:-unknown}. The matcher is still attached and announced; the lister is not."
    fi
    echo "ok=$ok" >> "${GITHUB_OUTPUT:-/dev/null}"
    echo "status=$status" >> "${GITHUB_OUTPUT:-/dev/null}"
    echo "surface lister: $status"
    ;;
  upload-matcher)
    tag="${2:?usage: release-artifacts.sh upload-matcher <tag>}"
    "$GH" release upload "$tag" \
      wasm-artifact/carrick_match.js \
      wasm-artifact/carrick_match.d.ts \
      wasm-artifact/carrick_match_bg.wasm \
      wasm-artifact/carrick_match.sha256 \
      --clobber --repo "${GITHUB_REPOSITORY:-carrick-tools/carrick}"
    ;;
  upload-lister)
    tag="${2:?usage: release-artifacts.sh upload-lister <tag>}"
    if ! lister_ships; then
      echo "surface lister not attached"
      exit 0
    fi
    "$GH" release upload "$tag" "$LISTER_ENTRY" "$LISTER_MANIFEST" \
      --clobber --repo "${GITHUB_REPOSITORY:-carrick-tools/carrick}"
    ;;
  dispatch)
    tag="${2:?usage: release-artifacts.sh dispatch <tag>}"
    js_sha="$(awk '$2 == "carrick_match.js" {print $1}' wasm-artifact/carrick_match.sha256)"
    dts_sha="$(awk '$2 == "carrick_match.d.ts" {print $1}' wasm-artifact/carrick_match.sha256)"
    wasm_sha="$(awk '$2 == "carrick_match_bg.wasm" {print $1}' wasm-artifact/carrick_match.sha256)"
    lister=()
    # Announced only when it is attached: the cloud fetches what it is told.
    if lister_ships && [ "${LISTER_ATTACHED:-}" = "success" ]; then
      manifest_sha="$(sha256sum "$LISTER_MANIFEST" | awk '{print $1}')"
      lister=(-f "client_payload[lister_manifest_sha256]=$manifest_sha")
    fi
    "$GH" api "repos/carrick-tools/carrick-cloud/dispatches" \
      -f event_type=carrick-match-release \
      -f "client_payload[tag]=$tag" \
      -f "client_payload[source_sha]=${SOURCE_SHA:?SOURCE_SHA is required}" \
      -f "client_payload[js_sha256]=$js_sha" \
      -f "client_payload[dts_sha256]=$dts_sha" \
      -f "client_payload[wasm_sha256]=$wasm_sha" \
      ${lister[@]+"${lister[@]}"}
    ;;
  *)
    echo "usage: release-artifacts.sh upload-matcher <tag> | lister-status | upload-lister <tag> | dispatch <tag>" >&2
    exit 2
    ;;
esac
