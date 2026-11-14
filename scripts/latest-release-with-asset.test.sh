#!/usr/bin/env bash
# Tests for scripts/latest-release-with-asset.sh (carrick#4451), gh stubbed.
# Wired into ci.yml (release-artifact-steps). Run from anywhere.
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
fail=0
ok()  { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# The stub lists carrick-v9.9.9 (no assets), v9.9.8 (assets), v9.9.7 (assets).
cat >"$work/gh" <<'STUB'
#!/usr/bin/env bash
case "$1 $2" in
  "release list") printf 'carrick-v9.9.9\ncarrick-v9.9.8\ncarrick-v9.9.7\n' ;;
  "release view")
    case "$3" in
      carrick-v9.9.9) [ -n "${ALL_HAVE_ASSET:-}" ] && echo carrick-action-linux.tar.gz ;;
      carrick-v9.9.8|carrick-v9.9.7) echo carrick-action-linux.tar.gz; echo other.js ;;
    esac ;;
esac
exit 0
STUB
chmod +x "$work/gh"
run() { PATH="$work:$PATH" "$root/scripts/latest-release-with-asset.sh" carrick-tools/carrick "$1" 2>/dev/null; }

got=$(run carrick-action-linux.tar.gz)
[ "$got" = "9.9.8" ] && ok "newest has no asset: picks the one before" || bad "want 9.9.8, got '$got'"
got=$(ALL_HAVE_ASSET=1 run carrick-action-linux.tar.gz)
[ "$got" = "9.9.9" ] && ok "newest has the asset: picks it" || bad "want 9.9.9, got '$got'"
if run missing.tar.gz >/dev/null; then bad "no release has the asset: must fail"; else ok "no release has the asset: fails"; fi
exit $fail
