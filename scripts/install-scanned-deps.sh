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
#   - Node installs are lockfile-gated; Deno uses its declared configuration
#     in frozen mode, including projects that disable the lockfile;
#   - lifecycle scripts disabled everywhere, so nothing in the scanned repo
#     executes during a scan;
#   - time-boxed: an install that fails or runs long prints a `::warning::`
#     and the scan continues, where the pre-flight then refuses that service
#     rather than indexing `any` through it (src/preflight.rs).
#
# Every service, not the scan root alone (carrick#1312). The pre-flight
# refuses PER SERVICE — a lockfile reachable from a service root with no
# `node_modules` under it — so an installer that prepares the root alone
# leaves a repo whose services carry their own lockfiles refused for every
# nested one. The set installed here is the set that check walks:
#
#   - the services come from `carrick derive`, which resolves them with the same
#     `service_derivation::resolve` the scan hands to the pre-flight (src/main.rs), so
#     `carrick.json` services and inferred workspace members are both covered
#     without a second derivation living in this file;
#   - the scan root is always in the set, so a checkout whose services cannot
#     be derived is still prepared exactly as it was before;
#   - each service root walks UP to the nearest lockfile, which is where the
#     install runs. Hoisting falls out of that walk: every member of a pnpm
#     workspace reaches the one lockfile at the workspace root, so the set
#     dedupes to a single install.
#
# Same-level precedence mirrors the sidecar's `lockfileVersions`
# (src/sidecar/src/capture/lockfile.ts) so the install and the type capture
# agree on which manager a directory uses.
#
# Usage:
#   install-scanned-deps.sh detect      <scan-root>          # key=value lines
#   install-scanned-deps.sh install     <scan-root>          # always exits 0
#   install-scanned-deps.sh install-one <root> <manager>     # always exits 0
#
# CARRICK_CLI names the Carrick to ask for the services. `action.yml` writes a
# wrapper for whichever copy the run resolved, npm package or release asset.
# Deliberately NOT `CARRICK_BIN`: that variable names a scanner BINARY to the
# npm package, which would resolve the wrapper as its own binary and re-enter
# it forever.
set -uo pipefail

# How long an install may take before it is killed and the scan continues bare.
INSTALL_TIMEOUT_SECONDS="${CARRICK_INSTALL_TIMEOUT:-300}"

# The delimiter for the one multi-line step output this script writes. A
# workflow reads `cache_dirs` as the `path:` of actions/cache, which takes one
# directory per line, and a plain `key=value` line cannot carry that.
CACHE_DIRS_DELIMITER="CARRICK_CACHE_DIRS"

# On stderr, not stdout: `detect` writes step outputs to stdout and the runner
# fails a step for any GITHUB_OUTPUT line that is not `key=value`. Workflow
# commands are honoured on either stream.
warn() { echo "::warning::$*" >&2; }

absolute() { (cd "$1" 2>/dev/null && pwd -P); }

# The manager named by the lockfile in ONE directory, in the sidecar's
# precedence order. Prints nothing when that directory carries no lockfile.
detect_manager() {
  local root="$1"
  if [ -f "$root/package-lock.json" ]; then echo "npm:$root/package-lock.json"
  elif [ -f "$root/pnpm-lock.yaml" ]; then echo "pnpm:$root/pnpm-lock.yaml"
  elif [ -f "$root/yarn.lock" ]; then echo "yarn:$root/yarn.lock"
  elif [ -f "$root/bun.lock" ]; then echo "bun:$root/bun.lock"
  elif [ -f "$root/bun.lockb" ]; then echo "bun:$root/bun.lockb"
  elif [ -f "$root/deno.json" ] || [ -f "$root/deno.jsonc" ]; then
    if [ -f "$root/deno.lock" ]; then echo "deno:$root/deno.lock"
    elif [ -f "$root/deno.json" ]; then echo "deno:$root/deno.json"
    else echo "deno:$root/deno.jsonc"; fi
  fi
}

# The Deno configuration a directory carries, in Deno's own order.
deno_config() {
  if [ -f "$1/deno.json" ]; then echo "$1/deno.json"
  elif [ -f "$1/deno.jsonc" ]; then echo "$1/deno.jsonc"
  fi
}

# Every service root the scan will visit, absolute, one per line.
#
# The scan root leads, and is emitted whatever the scanner answers: a checkout
# the derivation cannot read is still installed the way it was before this
# walked services at all.
service_roots() {
  local scan_root="$1" derived parsed
  printf '%s\n' "$scan_root"
  if [ -z "${CARRICK_CLI:-}" ]; then
    return 0
  fi
  derived=$("$CARRICK_CLI" derive --workspace "$scan_root" 2>/dev/null)
  if [ -z "$derived" ]; then
    warn "Carrick could not derive this repository's services, so only $scan_root is prepared; a service with its own lockfile will be refused as uninstalled."
    return 0
  fi
  # `carrick.derive/0`: one entry per repo, each service's `directory`
  # relative to that repo's path. A service that names no directory is the
  # repo itself.
  parsed=$(printf '%s' "$derived" | node -e '
    const path = require("path");
    let text = "";
    process.stdin.on("data", (chunk) => (text += chunk));
    process.stdin.on("end", () => {
      try {
        const derived = JSON.parse(text);
        for (const repo of derived.repos || []) {
          for (const service of repo.services || []) {
            console.log(path.resolve(repo.path, service.directory || "."));
          }
        }
      } catch {}
    });
  ' 2>/dev/null)
  # Node reads the answer, and `action.yml` sets Node up before this runs. A
  # runner without it, or a document this does not understand, must say so
  # rather than look like a repository with one service.
  if [ -z "$parsed" ]; then
    warn "Carrick named this repository's services but none could be read here, so only $scan_root is prepared; a service with its own lockfile will be refused as uninstalled."
    return 0
  fi
  printf '%s\n' "$parsed"
}

# Where the install for ONE service root runs: the nearest directory at or
# above it that carries a lockfile, stopping at the scan root.
#
# The same rule `preflight::dependencies_unprepared` applies, including its
# stop: a monorepo root with a lockfile of its own is not an install of a
# nested workspace that has one too, so the walk takes the first answer.
# Prints `manager<TAB>install-root<TAB>lockfile`, or nothing.
nearest_lockfile() {
  local service_root="$1" scan_root="$2" dir found
  dir="$service_root"
  while :; do
    found=$(detect_manager "$dir")
    if [ -n "$found" ]; then
      printf '%s\t%s\t%s\n' "${found%%:*}" "$dir" "${found#*:}"
      return 0
    fi
    [ "$dir" = "$scan_root" ] && return 1
    case "$dir" in "$scan_root"/*) ;; *) return 1 ;; esac
    dir=$(dirname "$dir")
  done
}

# Whether an install already covers this service: `node_modules` anywhere from
# the service root up to the install root, which is what the pre-flight reads.
# A hoisted member has none of its own and is prepared by the root's.
already_installed() {
  local service_root="$1" install_root="$2" dir="$1"
  while :; do
    [ -d "$dir/node_modules" ] && return 0
    [ "$dir" = "$install_root" ] && return 1
    case "$dir" in "$install_root"/*) ;; *) return 1 ;; esac
    dir=$(dirname "$dir")
  done
}

# The installs this checkout needs, deduped, one `manager<TAB>root<TAB>lockfile`
# line each, sorted so a cache key over them is stable.
install_plan() {
  local scan_root="$1" service_root row manager install_root
  while IFS= read -r service_root; do
    [ -n "$service_root" ] && [ -d "$service_root" ] || continue
    service_root=$(absolute "$service_root") || continue
    [ -n "$service_root" ] || continue
    row=$(nearest_lockfile "$service_root" "$scan_root") || continue
    IFS=$'\t' read -r manager install_root _ <<<"$row"
    # Deno caches outside the tree, so `node_modules` is no evidence that its
    # dependencies are there: a Deno root is always prepared, and a Node
    # install already on disk does not prove the SEPARATE Deno cache is warm
    # either, so a mixed root that has one is still prepared for Deno.
    if [ "$manager" = "deno" ]; then
      printf '%s\n' "$row"
      continue
    fi
    if [ -d "$install_root/node_modules" ] &&
        { [ -f "$install_root/deno.json" ] || [ -f "$install_root/deno.jsonc" ]; }; then
      printf 'deno\t%s\t%s\n' "$install_root" "$(deno_config "$install_root")"
      continue
    fi
    already_installed "$service_root" "$install_root" && continue
    printf '%s\n' "$row"
  done < <(service_roots "$scan_root" | sort -u) | sort -u
}

# The manager's own download store, which is what gets cached between runs.
# `node_modules` is deliberately not cached: `npm ci` deletes it before it
# installs, and a cache saved from a failed install would poison the next run,
# whereas a partial store is integrity-checked by the manager and harmless.
# An empty answer means "cache nothing"; it is never an error.
cache_dir_for() {
  local manager="$1" dir=""
  case "$manager" in
    deno)
      dir=$(deno info --json 2>/dev/null | node -e 'let text="";process.stdin.on("data",c=>text+=c);process.stdin.on("end",()=>{try{console.log(JSON.parse(text).denoDir||"")}catch{}})' || true)
      ;;
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

sha256_of_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 | cut -d' ' -f1
  fi
}

cmd_detect() {
  local root="$1" plan managers lockfiles sums manager install_root relative

  root=$(absolute "$root")
  if [ -z "$root" ]; then
    echo "should_install=false"
    echo "reason=scan root $1 does not exist"
    return 0
  fi

  plan=$(install_plan "$root")
  if [ -z "$plan" ]; then
    echo "should_install=false"
    echo "reason=no service of this repository has a lockfile that is not installed"
    return 0
  fi

  # Hyphen-joined, never comma-joined: `actions/cache` rejects a key holding
  # a comma outright, and a mixed npm+deno repo would fail the cache step.
  managers=$(printf '%s\n' "$plan" | cut -f1 | sort -u | paste -sd- -)
  lockfiles=$(printf '%s\n' "$plan" | cut -f3)
  # The key covers EVERY lockfile the run installs from: a repo whose nested
  # service changed its dependencies must not restore a store keyed on the
  # root's unchanged lockfile alone.
  sums=$(while IFS= read -r lockfile; do sha256_of "$lockfile"; done <<<"$lockfiles" | sort | tr -d '\n')

  # A log line, not a step output. The one thing a user debugging a pre-flight
  # refusal needs is WHICH roots this prepared, and a count does not say.
  while IFS=$'\t' read -r manager install_root _; do
    [ -n "$manager" ] || continue
    relative="${install_root#"$root"}"
    relative="${relative#/}"
    echo "Carrick prepares ${relative:-the repository root} with $manager." >&2
  done <<<"$plan"

  echo "should_install=true"
  echo "roots=$(printf '%s\n' "$plan" | wc -l | tr -d ' ')"
  echo "managers=$managers"
  echo "lockfiles_sha256=$(printf '%s' "$sums" | sha256_of_stdin)"
  # The one multi-line output: `actions/cache` reads it as `path:`, which takes
  # a directory per line, and GitHub carries that only in the heredoc form.
  echo "cache_dirs<<$CACHE_DIRS_DELIMITER"
  printf '%s\n' "$plan" | cut -f1 | sort -u | while IFS= read -r manager; do
    cache_dir_for "$manager"
  done | sort -u
  echo "$CACHE_DIRS_DELIMITER"
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

cmd_install_one() {
  local root="$1" manager="$2" log status
  if [ "$manager" != "deno" ] && { [ -f "$root/deno.json" ] || [ -f "$root/deno.jsonc" ]; }; then
    # Mixed roots need the global Deno cache AND the Node compiler's local
    # install. Neither preparation executes authorized project lifecycle code.
    cmd_install_one "$root" deno
  fi
  log=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/carrick-dependency-install.XXXXXX") || return 1

  # corepack ships with the Node the Action set up; it is how pnpm and yarn
  # reach the version the repo pins in `packageManager`.
  corepack enable >/dev/null 2>&1 || true

  case "$manager" in
    deno)
      # A project can authorize lifecycle scripts through allowScripts in its
      # config. Deno has no --ignore-scripts: disabling node_modules is what
      # prevents those scripts from running, even for an authorizing config.
      # Frozen mode also prevents rewriting the project's lockfile.
      run_install "$root" "$log" deno install --frozen --node-modules-dir=none
      status=$?
      ;;
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
    warn "Carrick's dependency install ($manager) in $root hit the ${INSTALL_TIMEOUT_SECONDS}s limit; the scan will refuse that service rather than index \`any\` through its packages."
  else
    warn "Carrick's dependency install ($manager) in $root failed (exit $status); the scan will refuse that service rather than index \`any\` through its packages."
  fi
  return 0
}

# Every root the pre-flight will ask about, in one step. One root's failure
# does not stop the others: a repo whose thirteenth service cannot install
# still gets the twelve that can.
cmd_install() {
  local root row manager install_root
  root=$(absolute "$1")
  if [ -z "$root" ]; then
    warn "Carrick's dependency install found no directory at $1; continuing on the bare checkout."
    return 0
  fi
  while IFS= read -r row; do
    [ -n "$row" ] || continue
    IFS=$'\t' read -r manager install_root _ <<<"$row"
    cmd_install_one "$install_root" "$manager"
  done < <(install_plan "$root")
}

main() {
  local action="${1:-}"
  case "$action" in
    detect)
      [ $# -eq 2 ] || { echo "usage: $0 detect <scan-root>" >&2; exit 2; }
      cmd_detect "$2"
      ;;
    install)
      [ $# -eq 2 ] || { echo "usage: $0 install <scan-root>" >&2; exit 2; }
      cmd_install "$2"
      ;;
    install-one)
      [ $# -eq 3 ] || { echo "usage: $0 install-one <root> <manager>" >&2; exit 2; }
      cmd_install_one "$2" "$3"
      ;;
    *)
      echo "usage: $0 {detect|install|install-one} ..." >&2
      exit 2
      ;;
  esac
}

main "$@"
