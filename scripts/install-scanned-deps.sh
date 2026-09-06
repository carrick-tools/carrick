#!/usr/bin/env bash
#
# Install the scanned repository's dependencies so the type layer sees real
# types instead of `error`.
#
# On a bare checkout every import from a package resolves to the compiler's
# `error` type and the printed endpoint type is `any`, so the index records a
# route it cannot describe. `action.yml` calls this before "Run analysis".
#
# Posture (carrick#706):
#   - lockfile-gated: no lockfile at the scan root, no install;
#   - lifecycle scripts disabled everywhere, so nothing in the scanned repo
#     executes during a scan;
#   - time-boxed and tolerant: a failed or slow install prints a `::warning::`
#     and the scan continues on the bare checkout exactly as before;
#   - the scan root only, never a walk up to a hoisted workspace root: the
#     sidecar reads `<scan root>/node_modules` when it decides `bare_checkout`,
#     so installing anywhere else would report installed and still capture
#     bare.
#
# Same-level precedence mirrors the sidecar's `lockfileVersions`
# (src/sidecar/src/capture/lockfile.ts) so the install and the type capture
# agree on which manager the repo uses.
#
# Usage:
#   install-scanned-deps.sh detect  <scan-root>            # key=value lines
#   install-scanned-deps.sh install <scan-root> <manager>  # always exits 0
set -uo pipefail

# How long an install may take before it is killed and the scan continues bare.
INSTALL_TIMEOUT_SECONDS="${CARRICK_INSTALL_TIMEOUT:-300}"

warn() { echo "::warning::$*"; }

# The manager named by the nearest lockfile at the scan root, in the sidecar's
# precedence order. Prints nothing when the root carries no lockfile.
detect_manager() {
  local root="$1"
  if [ -f "$root/package-lock.json" ]; then echo "npm:$root/package-lock.json"
  elif [ -f "$root/pnpm-lock.yaml" ]; then echo "pnpm:$root/pnpm-lock.yaml"
  elif [ -f "$root/yarn.lock" ]; then echo "yarn:$root/yarn.lock"
  elif [ -f "$root/bun.lock" ]; then echo "bun:$root/bun.lock"
  elif [ -f "$root/bun.lockb" ]; then echo "bun:$root/bun.lockb"
  fi
}

# The manager's own download store, which is what gets cached between runs.
# `node_modules` is deliberately not cached: `npm ci` deletes it before it
# installs, and a cache saved from a failed install would poison the next run,
# whereas a partial store is integrity-checked by the manager and harmless.
# An empty answer means "cache nothing"; it is never an error.
cache_dir_for() {
  local manager="$1" dir=""
  case "$manager" in
    npm)
      command -v npm >/dev/null 2>&1 && dir=$(npm config get cache 2>/dev/null || true)
      ;;
    pnpm)
      dir=$(corepack pnpm store path --silent 2>/dev/null | tail -n 1 || true)
      ;;
    yarn)
      if command -v yarn >/dev/null 2>&1; then
        dir=$(yarn config get cacheFolder 2>/dev/null | tail -n 1 || true)
        case "$dir" in /*) ;; *) dir=$(yarn cache dir 2>/dev/null | tail -n 1 || true) ;; esac
      fi
      ;;
    bun)
      command -v bun >/dev/null 2>&1 && dir="${HOME}/.bun/install/cache"
      ;;
  esac
  # Managers print `undefined`, `null` or an error line when they cannot
  # answer. Only an absolute path is a cache directory.
  case "$dir" in /*) echo "$dir" ;; esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

cmd_detect() {
  local root="$1" found manager lockfile

  if [ ! -d "$root" ]; then
    echo "should_install=false"
    echo "reason=scan root $root does not exist"
    return 0
  fi

  found=$(detect_manager "$root")
  if [ -z "$found" ]; then
    echo "should_install=false"
    echo "reason=no lockfile at the scan root"
    return 0
  fi
  manager="${found%%:*}"
  lockfile="${found#*:}"

  if [ -d "$root/node_modules" ]; then
    echo "should_install=false"
    echo "manager=$manager"
    echo "reason=node_modules already present at the scan root"
    return 0
  fi

  echo "should_install=true"
  echo "manager=$manager"
  echo "lockfile=$lockfile"
  echo "lockfile_sha256=$(sha256_of "$lockfile")"
  echo "cache_dir=$(cache_dir_for "$manager")"
}

# Run one install command with scripts disabled, time-boxed, output to a log
# file rather than a pipe: a pipeline reports the last command's status, so a
# failed install piped to `tail` would count as success.
run_install() {
  local root="$1" log="$2"
  shift 2
  # `timeout` is GNU coreutils: present on the runners, absent on a stock
  # macOS dev box. Without it the install still runs, just unbounded.
  if command -v timeout >/dev/null 2>&1; then
    ( cd "$root" && timeout -k 10 "$INSTALL_TIMEOUT_SECONDS" "$@" >"$log" 2>&1 )
  else
    ( cd "$root" && "$@" >"$log" 2>&1 )
  fi
}

cmd_install() {
  local root="$1" manager="$2" log status
  log="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/carrick-dependency-install.log"

  # corepack ships with the Node the Action set up; it is how pnpm and yarn
  # reach the version the repo pins in `packageManager`.
  corepack enable >/dev/null 2>&1 || true

  case "$manager" in
    npm)
      run_install "$root" "$log" npm ci --ignore-scripts
      status=$?
      ;;
    pnpm)
      run_install "$root" "$log" corepack pnpm install --frozen-lockfile --ignore-scripts
      status=$?
      ;;
    yarn)
      # Berry understands `--mode=skip-build`; classic understands
      # `--ignore-scripts`. Each rejects the other's flag, so try both.
      run_install "$root" "$log" yarn install --mode=skip-build
      status=$?
      if [ $status -ne 0 ]; then
        run_install "$root" "$log" yarn install --ignore-scripts
        status=$?
      fi
      ;;
    bun)
      if ! command -v bun >/dev/null 2>&1; then
        warn "Carrick found a bun lockfile but bun is not installed on this runner; add oven-sh/setup-bun before the Carrick step, or types through dependencies stay \`any\`."
        return 0
      fi
      run_install "$root" "$log" bun install --ignore-scripts
      status=$?
      ;;
    *)
      warn "Carrick does not know how to install with '$manager'; continuing on the bare checkout."
      return 0
      ;;
  esac

  if [ $status -eq 0 ]; then
    echo "Installed $root dependencies with $manager (lifecycle scripts disabled)."
    [ -f "$log" ] && tail -n 3 "$log"
    return 0
  fi

  [ -f "$log" ] && tail -n 20 "$log"
  if [ $status -eq 124 ] || [ $status -eq 137 ]; then
    warn "Carrick's dependency install ($manager) hit the ${INSTALL_TIMEOUT_SECONDS}s limit; continuing on the bare checkout, so types resolved through dependencies will be \`any\`."
  else
    warn "Carrick's dependency install ($manager) failed (exit $status); continuing on the bare checkout, so types resolved through dependencies will be \`any\`."
  fi
  return 0
}

main() {
  local action="${1:-}"
  case "$action" in
    detect)
      [ $# -eq 2 ] || { echo "usage: $0 detect <scan-root>" >&2; exit 2; }
      cmd_detect "$2"
      ;;
    install)
      [ $# -eq 3 ] || { echo "usage: $0 install <scan-root> <manager>" >&2; exit 2; }
      cmd_install "$2" "$3"
      ;;
    *)
      echo "usage: $0 {detect|install} ..." >&2
      exit 2
      ;;
  esac
}

main "$@"
