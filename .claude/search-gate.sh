#!/bin/sh
# Stops the FIRST repo-wide search of a session and nothing after it.
# Fails OPEN on any internal error (no jq, malformed payload, unwritable
# marker): a gate that cannot read its input must not become a block.
payload=$(cat 2>/dev/null) || exit 0
tool=$(printf %s "$payload" | jq -r '.tool_name // ""' 2>/dev/null) || exit 0
case "$tool" in
  Grep) ;;
  Bash)
    # A tool-level matcher is not enough on its own: an agent denied Grep
    # reaches for grep through the shell. Matched on the command text so an
    # ordinary non-search Bash call passes through and does not spend the nudge.
    cmd=$(printf %s "$payload" | jq -r '.tool_input.command // ""' 2>/dev/null) || exit 0
    printf %s "$cmd" | grep -qE '(^|[^[:alnum:]_.-])(grep|egrep|fgrep|rg|ag|ack)([[:space:]]|$)' || exit 0
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
