#!/usr/bin/env bash
# Same-key commits on main inherit the box's `release-check` verdict
# (carrick-cloud#2916 sub-ticket B; run by .github/workflows/release-check-inherit.yml).
#
# The replay box checks main once per scanner change and posts the commit
# status `release-check` on the checked commit. A later commit whose scanner key
# (scripts/scanner-key.sh) is the same, for example a docs merge, never gets a
# check of its own. This script is one idempotent reconcile, whatever started
# it, over the last INHERIT_DEPTH first-parent commits of INHERIT_REF, oldest
# first:
#   - a commit with a direct status (description does not start "Same scanner
#     as ") keeps it;
#   - any other commit takes the state of its nearest first-parent ancestor that
#     has a direct status and the same key, with the description
#     "Same scanner as <sha8>: <that description>" cut to 140 characters;
#   - a post happens only when the commit's newest `release-check` differs, and
#     none happens when no such ancestor exists.
# No status other than `release-check` is read or written.
#
# Environment: GITHUB_REPOSITORY (owner/repo), GH (the gh command, a test seam),
# INHERIT_DEPTH (default 30), INHERIT_REF (default HEAD).
set -u
here="$(cd "$(dirname "$0")" && pwd)"
GH="${GH:-gh}"
repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
depth="${INHERIT_DEPTH:-30}"
ref="${INHERIT_REF:-HEAD}"
context=release-check
prefix="Same scanner as "

shas=()
while IFS= read -r line; do shas+=("$line"); done < <(git rev-list --first-parent --max-count="$depth" "$ref" | sed '1!G;h;$!d')

keys=(); dstate=(); ddesc=(); cstate=(); cdesc=()
for sha in "${shas[@]}"; do
  key=$(bash "$here/scanner-key.sh" "$sha") || key="unknown-$sha"
  json=$("$GH" api "repos/$repo/commits/$sha/statuses" --paginate) || { echo "release-check-inherit: cannot read statuses of $sha" >&2; exit 1; }
  # Newest first. Current: newest release-check. Direct: newest one not inherited.
  cur=$(jq -r --arg c "$context" '[.[] | select(.context == $c)] | (.[0] // {}) | [.state // "", .description // ""] | @tsv' <<<"$json")
  dir=$(jq -r --arg c "$context" --arg p "$prefix" '[.[] | select(.context == $c and ((.description // "") | startswith($p) | not))] | (.[0] // {}) | [.state // "", .description // ""] | @tsv' <<<"$json")
  keys+=("$key")
  cstate+=("${cur%%$'\t'*}"); cdesc+=("${cur#*$'\t'}")
  dstate+=("${dir%%$'\t'*}"); ddesc+=("${dir#*$'\t'}")
done

posted=0
for ((i = 0; i < ${#shas[@]}; i++)); do
  [ -n "${dstate[i]}" ] && continue   # direct status: keep it
  src=-1
  for ((j = i - 1; j >= 0; j--)); do
    if [ "${keys[j]}" = "${keys[i]}" ] && [ -n "${dstate[j]}" ]; then src=$j; break; fi
  done
  [ "$src" -ge 0 ] || continue
  want_state="${dstate[src]}"
  want_desc=$(printf '%s%s: %s' "$prefix" "${shas[src]:0:8}" "${ddesc[src]}" | cut -c1-140)
  if [ "${cstate[i]}" = "$want_state" ] && [ "${cdesc[i]}" = "$want_desc" ]; then continue; fi
  "$GH" api -X POST "repos/$repo/statuses/${shas[i]}" \
    -f "state=$want_state" -f "context=$context" -f "description=$want_desc" >/dev/null \
    || { echo "release-check-inherit: post to ${shas[i]:0:8} failed" >&2; exit 1; }
  echo "inherit: ${shas[i]:0:8} <- ${shas[src]:0:8} $want_state"
  posted=$((posted + 1))
done
echo "release-check-inherit: $posted post(s) over ${#shas[@]} commit(s)"
