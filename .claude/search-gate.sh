#!/bin/sh
# Stops the FIRST repo-wide search of a session and nothing after it.
# Fails OPEN on any internal error (no jq, malformed payload, unwritable
# marker): a gate that cannot read its input must not become a block.
payload=$(cat 2>/dev/null) || exit 0
tool=$(printf %s "$payload" | jq -r '.tool_name // ""' 2>/dev/null) || exit 0
# The session's working directory, so a search of somewhere else can be told
# apart from a sweep of the repo. Falls back to $PWD when the payload has none.
cwd=$(printf %s "$payload" | jq -r '.cwd // ""' 2>/dev/null) || exit 0
[ -n "$cwd" ] || cwd=$PWD
# True for an EXISTING absolute path that is not under the working directory.
# Such a path is not a repo-wide search: a large tool result overflows to a
# file under $TMPDIR, and reading that file back is recovery, not the
# vocabulary sweep this gate exists to interrupt once. Existence is what
# separates a path from a pattern that merely looks like one, so a search
# for "/api/v1/whoami" across the repo is still gated.
outside_cwd() {
  [ -e "$1" ] || return 1
  case "$1" in
    "$cwd"|"$cwd"/*) return 1 ;;
    /*) return 0 ;;
    *) return 1 ;;
  esac
}
case "$tool" in
  Grep)
    path=$(printf %s "$payload" | jq -r '.tool_input.path // ""' 2>/dev/null) || exit 0
    outside_cwd "$path" && exit 0
    ;;
  Bash)
    # A tool-level matcher is not enough on its own: an agent denied Grep
    # reaches for grep through the shell. Matched on the command text so an
    # ordinary non-search Bash call passes through and does not spend the nudge.
    cmd=$(printf %s "$payload" | jq -r '.tool_input.command // ""' 2>/dev/null) || exit 0
    printf %s "$cmd" | grep -qE '(^|[^[:alnum:]_.-])(grep|egrep|fgrep|rg|ag|ack)([[:space:]]|$)' || exit 0
    # Quotes are dropped (octal, so the shell quoting stays readable) and
    # globbing is off, so each word can be tested as a path on its own. A
    # path containing a space splits into words that exist nowhere and the
    # gate nudges, which is its designed behaviour rather than a failure.
    scan=$(printf %s "$cmd" | tr -d '\042\047')
    set -f
    for tok in $scan; do
      outside_cwd "$tok" && exit 0
    done
    set +f
    ;;
  *) exit 0 ;;
esac
# Session-scoped marker, outside the repo, so the nudge happens once per
# session and no marker file is ever committed.
sid=$(printf %s "$payload" | jq -r '.session_id // ""' 2>/dev/null) || exit 0
[ -n "$sid" ] || exit 0
marker="${TMPDIR:-/tmp}/carrick-search-nudged-$sid"
[ -e "$marker" ] && exit 0
touch "$marker" 2>/dev/null || exit 0
# Exit 2 blocks that one call and hands the message to the model.
echo "Prior art first: run mcp__carrick__search_by_intent with a plain-English description of the behaviour you are looking for, then run this search if you still need it. This is the only time this will interrupt you." >&2
exit 2
