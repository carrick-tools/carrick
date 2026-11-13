#!/usr/bin/env bash
# Refresh npm/carrick/package-lock.json's platform entries to a published
# version and open the PR (carrick#3789, #4137). Called by release.yml, job
# `lock-refresh` (after a publish) and job `lock-refresh-only` (the
# workflow_dispatch `lock_refresh_version` path). Tests:
# scripts/npm-lock-refresh.test.py.
#
#   VERSION=<x.y.z> GH_TOKEN=<token> [DRY_RUN=true] scripts/npm-lock-refresh-pr.sh
#
# Run from a checkout of main. DRY_RUN=true writes the refreshed entries,
# prints the diff and stops: no branch, no push, no PR, and the "main's
# lockfile is at this version" guard is not applied, so it can be pointed at
# any already-published version.
set -euo pipefail
LOCK=npm/carrick/package-lock.json
: "${VERSION:?VERSION is required}"
case "$VERSION" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "::error::VERSION must look like 1.2.3, got: $VERSION"; exit 1 ;;
esac
DRY_RUN="${DRY_RUN:-false}"

HAVE=$(node -p "require('./$LOCK').version")
if [ "$DRY_RUN" != "true" ] && [ "$HAVE" != "$VERSION" ]; then
  echo "::notice::main's lockfile is at $HAVE, not $VERSION; a newer release owns the refresh."
  exit 0
fi
python3 scripts/npm-lock-refresh.py "$VERSION" "$LOCK"
if git diff --quiet -- "$LOCK"; then
  echo "The lockfile already carries the $VERSION entries; nothing to open."
  exit 0
fi
if [ "$DRY_RUN" = "true" ]; then
  git --no-pager diff --stat -- "$LOCK"
  git --no-pager diff -- "$LOCK" | head -80
  echo "::notice::dry run: refreshed the entries to $VERSION and stopped before any branch or PR."
  exit 0
fi
BRANCH="lock-refresh-$VERSION"
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git checkout -b "$BRANCH"
git add "$LOCK"
git commit -m "chore(npm): refresh lockfile platform entries to $VERSION" \
  -m "Mechanical: the five @carrick-tools/cli-* entries take the published version's resolved URL and integrity (carrick#3789)."
git push --force origin "$BRANCH"
gh pr create --base main --head "$BRANCH" \
  --title "chore(npm): refresh lockfile platform entries to $VERSION" \
  --body "Mechanical, opened by release.yml after $VERSION was published. Only the five \`node_modules/@carrick-tools/cli-*\` entries change (version, resolved, integrity), from \`npm view\`. Merges on green. Refs #3789."
