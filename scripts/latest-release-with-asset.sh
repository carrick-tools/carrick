#!/usr/bin/env bash
# Prints the version (tag without `carrick-v`) of the newest published release
# that already carries <asset>. A release exists before its assets are
# uploaded, so "latest" alone can name one with nothing to download
# (carrick#4451). Usage: latest-release-with-asset.sh <owner/repo> <asset>.
# Used by the "Fetch the floor and latest releases" step in ci.yml.
set -eu
repo=$1
asset=$2
tags=$(gh release list -R "$repo" --exclude-drafts --exclude-pre-releases -L 20 --json tagName -q '.[].tagName')
for tag in $tags; do
  case "$tag" in carrick-v*) ;; *) continue ;; esac
  if gh release view "$tag" -R "$repo" --json assets -q '.assets[].name' | grep -qxF "$asset"; then
    echo "${tag#carrick-v}"
    exit 0
  fi
done
echo "no release in the newest 20 carries $asset" >&2
exit 1
