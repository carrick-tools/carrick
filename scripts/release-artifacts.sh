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
# The one list of type reasons (src/sidecar/src/capture/type-reasons.json,
# carrick#4231) rides with the matcher: copied byte for byte beside it, listed
# in carrick_match.sha256, attached, and announced by its sha256, so the cloud
# pins it in wasm-pin.json with the matcher's files and reads each reason's
# sentence and `decided` value from it. A tag from before the list has none;
# the matcher then ships alone, as before.
#
#   release-artifacts.sh checksums             copies the reasons list in when the tag has it, then
#                                              writes wasm-artifact/carrick_match.sha256
#   release-artifacts.sh upload-matcher <tag>  runs `gh release upload` (first, before the lister builds)
#   release-artifacts.sh lister-status         reads BUILD_OUTCOME, BUILT, TEST_OUTCOME;
#                                              writes ok= and status= to $GITHUB_OUTPUT
#   release-artifacts.sh upload-lister <tag>   reads LISTER_OK; uploads nothing unless it passed
#   release-artifacts.sh dispatch <tag>        reads LISTER_OK, LISTER_ATTACHED (the attach step's
#                                              outcome), SOURCE_SHA; runs `gh api`
#   release-artifacts.sh main-check <sha> <key>  tells carrick-cloud main's scanner key changed
#                                              (carrick-main-check); runs `gh api`
#   release-artifacts.sh announce-cli <tag>    reads SOURCE_SHA; tells carrick-cloud npm now serves
#                                              this version (carrick-cli-published); runs `gh api`
#
# Paths are relative to the working directory: wasm-artifact/ (matcher) and
# lister-artifact/ (lister). GH names the gh binary (a stub in the test).
set -euo pipefail

GH="${GH:-gh}"
LISTER_ENTRY="lister-artifact/carrick-lister.mjs"
LISTER_MANIFEST="lister-artifact/carrick-lister.manifest.json"
REASONS_SOURCE="src/sidecar/src/capture/type-reasons.json"
REASONS_ASSET="type-reasons.json"

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
  checksums)
    matcher_files=(carrick_match.js carrick_match.d.ts carrick_match_bg.wasm)
    if [ -f "$REASONS_SOURCE" ]; then
      cp "$REASONS_SOURCE" "wasm-artifact/$REASONS_ASSET"
      matcher_files+=("$REASONS_ASSET")
    else
      echo "No $REASONS_SOURCE at this tag; the matcher ships without the type reasons list."
    fi
    (cd wasm-artifact && sha256sum "${matcher_files[@]}" > carrick_match.sha256)
    ;;
  upload-matcher)
    tag="${2:?usage: release-artifacts.sh upload-matcher <tag>}"
    reasons=()
    if [ -f "wasm-artifact/$REASONS_ASSET" ]; then
      reasons=("wasm-artifact/$REASONS_ASSET")
    fi
    "$GH" release upload "$tag" \
      wasm-artifact/carrick_match.js \
      wasm-artifact/carrick_match.d.ts \
      wasm-artifact/carrick_match_bg.wasm \
      wasm-artifact/carrick_match.sha256 \
      ${reasons[@]+"${reasons[@]}"} \
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
    reasons_sha="$(awk -v f="$REASONS_ASSET" '$2 == f {print $1}' wasm-artifact/carrick_match.sha256)"
    reasons=()
    if [ -n "$reasons_sha" ]; then
      reasons=(-f "client_payload[type_reasons_sha256]=$reasons_sha")
    fi
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
      ${reasons[@]+"${reasons[@]}"} \
      ${lister[@]+"${lister[@]}"}
    ;;
  announce-cli)
    tag="${2:?usage: release-artifacts.sh announce-cli <tag>}"
    if ! "$GH" api "repos/carrick-tools/carrick-cloud/dispatches" \
      -f event_type=carrick-cli-published \
      -f "client_payload[tag]=$tag" \
      -f "client_payload[version]=${tag#carrick-v}" \
      -f "client_payload[source_sha]=${SOURCE_SHA:?SOURCE_SHA is required}"; then
      echo "::error::carrick-cloud was not told $tag is on npm, so the runner's scanner pin has not moved. Backfill: gh workflow run scanner-pin-bump.yml -R carrick-tools/carrick-cloud -f tag=$tag"
      exit 1
    fi
    ;;
  main-check)
    sha="${2:?usage: release-artifacts.sh main-check <sha> <key>}"
    key="${3:?usage: release-artifacts.sh main-check <sha> <key>}"
    "$GH" api "repos/carrick-tools/carrick-cloud/dispatches" \
      -f event_type=carrick-main-check \
      -f "client_payload[sha]=$sha" \
      -f "client_payload[key]=$key"
    ;;
  *)
    echo "usage: release-artifacts.sh checksums | upload-matcher <tag> | lister-status | upload-lister <tag> | dispatch <tag> | announce-cli <tag> | main-check <sha> <key>" >&2
    exit 2
    ;;
esac
