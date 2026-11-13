#!/usr/bin/env bash
# The write steps of a release cut (release.yml `cut` job; plan R2: `release`
# is a pointer branch, and a push to it tags and publishes the sha it points at).
#
#   release-cut.sh <sha> <version> <pr> <release_commit>
#
# Run only after `scripts/release-cut-check.py --json <sha>` said cut (exit 0).
# The version, the merged version PR and its merge commit are that script's
# answer, passed through unchanged: this script decides nothing, it writes.
#
# Three writes, in order, each skipped when already done, so a run that stopped
# half way is resumed by running it again:
#   1. tag    carrick-v<version> at <sha> (a ref; a tag at another sha is an error)
#   2. Release carrick-v<version>: the CHANGELOG.md section for <version> at
#      <sha>, plus the first-parent commits merged after the version PR
#   3. relabel the version PR: add `autorelease: tagged`, then remove
#      `autorelease: pending`, LAST. release-please refuses to open the next
#      version PR while a merged one is pending, and release-cut-check.py
#      reads `pending` as "not finished", so a failure anywhere earlier leaves
#      the cut resumable.
#
# Needs a checkout holding <sha> and <release_commit> (fetch-depth 0).
# Environment: GITHUB_REPOSITORY (owner/repo), GH (the gh command, a test seam).
set -euo pipefail
GH="${GH:-gh}"
repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
[ $# -eq 4 ] || { echo "usage: release-cut.sh <sha> <version> <pr> <release_commit>" >&2; exit 2; }
sha=$1 version=$2 pr=$3 release_commit=$4

[[ $sha =~ ^[0-9a-f]{40}$ ]] || { echo "release-cut: '$sha' is not a full sha" >&2; exit 2; }
[[ $release_commit =~ ^[0-9a-f]{40}$ ]] || { echo "release-cut: release commit '$release_commit' is not a full sha" >&2; exit 2; }
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || { echo "release-cut: '$version' is not a version" >&2; exit 2; }
[[ $pr =~ ^[0-9]+$ ]] || { echo "release-cut: '$pr' is not a PR number" >&2; exit 2; }
tag="carrick-v$version"

# 1. Tag.
if at=$("$GH" api "repos/$repo/git/ref/tags/$tag" --jq '.object.sha' 2>/dev/null); then
  if [ "$at" != "$sha" ]; then
    echo "release-cut: $tag already points at $at, not $sha; nothing written" >&2
    exit 1
  fi
  echo "tag: $tag already at ${sha:0:8}"
else
  "$GH" api -X POST "repos/$repo/git/refs" -f "ref=refs/tags/$tag" -f "sha=$sha" >/dev/null
  echo "tag: created $tag at ${sha:0:8}"
fi

# 2. GitHub Release.
if "$GH" release view "$tag" --repo "$repo" >/dev/null 2>&1; then
  echo "release: $tag exists"
else
  notes=$(mktemp)
  trap 'rm -f "$notes"' EXIT
  # The version's section of CHANGELOG.md, header included, as release-please
  # wrote it: from `## [<version>]` up to the next `## [`.
  git show "$sha:CHANGELOG.md" | awk -v v="## [$version]" '
    index($0, "## [") == 1 { on = (index($0, v) == 1) }
    on { print }' >"$notes"
  if [ ! -s "$notes" ]; then
    echo "release-cut: CHANGELOG.md at ${sha:0:8} has no section for $version" >&2
    exit 1
  fi
  also=$(git log --first-parent --format='* %s (%h)' "$release_commit..$sha")
  if [ -n "$also" ]; then
    printf '\n### Also in this release (merged after the version bump, checked by main'"'"'s check at %s)\n\n%s\n' \
      "${sha:0:8}" "$also" >>"$notes"
  fi
  "$GH" release create "$tag" --repo "$repo" --verify-tag --title "carrick: v$version" \
    --notes-file "$notes" --latest=false >/dev/null
  echo "release: created $tag"
fi

# 3. Relabel, last.
labels=$("$GH" api "repos/$repo/issues/$pr/labels" --jq '.[].name')
if ! grep -qxF 'autorelease: tagged' <<<"$labels"; then
  "$GH" api -X POST "repos/$repo/issues/$pr/labels" -f 'labels[]=autorelease: tagged' >/dev/null
  echo "relabel: #$pr autorelease: tagged added"
fi
if grep -qxF 'autorelease: pending' <<<"$labels"; then
  "$GH" api -X DELETE "repos/$repo/issues/$pr/labels/autorelease:%20pending" >/dev/null
  echo "relabel: #$pr autorelease: pending removed"
else
  echo "relabel: #$pr is not pending"
fi
echo "cut: $tag at ${sha:0:8}"
