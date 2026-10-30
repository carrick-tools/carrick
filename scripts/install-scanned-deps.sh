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
# CARRICK_INSTALL_UNTRUSTED=1 is for a machine that scans repositories it does
# not own (the hosted runner). Lifecycle scripts being off is not enough there:
# a pnpm `.pnpmfile.cjs`, a Yarn `yarnPath` / `yarn-path` release and Yarn
# Berry `plugins` are all files from the scanned tree that run during an
# install, and so is any program a project `.npmrc` names (carrick#2246). In
# this mode none of them can:
#   - npm runs with every setting that names a program (`git`, `node-options`,
#     `script-shell`, `shell`, `node-gyp`) set in the environment, which
#     outranks the tree's `.npmrc` (see `npm_trusted_tools`);
#   - pnpm installs with `--ignore-pnpmfile`, which also stops the pnpmfile a
#     `configDependencies` package ships (pnpm 10 then fails the frozen
#     install on `pnpmfileChecksum` and warns, carrick#2251); no pnpm setting
#     in `.npmrc` or `pnpm-workspace.yaml` starts a program from the tree
#     during a frozen install without scripts (surveyed on pnpm 9 and 10,
#     carrick#2246);
#   - Yarn runs with YARN_IGNORE_PATH=1, so the launcher never hands the
#     install to a committed release, and Berry with YARN_ENABLE_SCRIPTS=0;
#   - a Yarn root where any `.yarnrc.yml` from it up to the clone root lists
#     `plugins`, or names a `yarnPath` with no `packageManager` pin (the
#     version cannot be known without running the repo's file), or whose
#     `yarn.lock` resolves a package from a git repository (Berry prepares it
#     with a package manager that reads the clone's own config, carrick#2251;
#     classic runs its `prepare`, carrick#2258) or with Berry's `exec:`
#     protocol (carrick#2257), is not installed: it warns and the pre-flight
#     refuses it. The scanner reads those files as YAML (`$CARRICK_CLI
#     __untrusted-yarn-refusal`), so this mode needs CARRICK_CLI for Yarn
#     roots and refuses them without it;
#   - corepack never writes `packageManager` into the repo's package.json
#     (COREPACK_ENABLE_AUTO_PIN=0), and classic Yarn and bun install frozen, so
#     no tracked file differs from the commit afterwards.
# Unset, the script behaves as it always has: on a developer's own CI the
# tree is theirs. `action.yml` leaves it unset.
#
# CARRICK_INSTALL_LOCKFILE_RETRY=1 is the hosted runner's lane policy (carrick#3069),
# separate from the posture above. Unset, nothing here changes. Set, a strict
# install that fails with anything but a timeout (124, 137) is retried once, with
# lifecycle scripts still disabled, the same time limit and every untrusted guard:
#   npm   npm ci                      -> npm install --ignore-scripts --no-audit --no-fund --no-save
#   pnpm  install --frozen-lockfile   -> install --no-frozen-lockfile --ignore-scripts [--ignore-pnpmfile]
#   yarn  classic as above            -> yarn install --pure-lockfile --ignore-scripts
#   bun   as above                    -> bun install --ignore-scripts --no-save
# Not retried: Yarn Berry (no install mode that reads the lockfile without writing
# it; zero-install repos commit what a mutable install writes) and Deno (the
# pre-flight asks only when `nodeModulesDir` is set, and no failing frozen
# install has been seen). The tree stays the commit: the lockfile, package.json
# and pnpm-workspace.yaml are restored byte for byte after the retry, and a root
# that ends uninstalled keeps no node_modules directory the attempts created.
#
# CARRICK_CLI names the Carrick to ask for the services. `action.yml` writes a
# wrapper for whichever copy the run resolved, npm package or release asset.
# Deliberately NOT `CARRICK_BIN`: that variable names a scanner BINARY to the
# npm package, which would resolve the wrapper as its own binary and re-enter
# it forever.
set -uo pipefail

# How long an install may take before it is killed and the scan continues bare.
INSTALL_TIMEOUT_SECONDS="${CARRICK_INSTALL_TIMEOUT:-300}"

# Set by a machine that scans repositories it does not own; see the header.
UNTRUSTED="${CARRICK_INSTALL_UNTRUSTED:-}"

# Set by the hosted runner; see the header. A lane policy, separate from UNTRUSTED.
LOCKFILE_RETRY="${CARRICK_INSTALL_LOCKFILE_RETRY:-}"

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
#
# Every directory is checked to sit inside the scan root BEFORE its lockfile
# is read, so a service root outside the checkout (carrick#2245) is never
# looked at, let alone installed.
nearest_lockfile() {
  local service_root="$1" scan_root="$2" dir found
  dir="$service_root"
  while :; do
    case "$dir" in "$scan_root" | "$scan_root"/*) ;; *) return 1 ;; esac
    found=$(detect_manager "$dir")
    if [ -n "$found" ]; then
      printf '%s\t%s\t%s\n' "${found%%:*}" "$dir" "${found#*:}"
      return 0
    fi
    [ "$dir" = "$scan_root" ] && return 1
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

# npm settings that name a program npm starts, each pinned to the machine's
# own answer for an untrusted install (carrick#2246). npm reads them from the
# tree's `.npmrc`, and the environment outranks that file, in this npm and in
# every npm it starts: a git dependency is cloned with `git`, and one whose
# manifest declares a build script is prepared by a child `npm install` that
# runs whatever `node-options` says, `--ignore-scripts` or not (pacote's
# `git.js`). `script-shell`, `shell` and `node-gyp` are the other settings that
# name a program; nothing `npm ci --ignore-scripts` does reaches them today.
# Sets NPM_TRUSTED_TOOLS to `env` assignments.
npm_trusted_tools() {
  local gyp
  NPM_TRUSTED_TOOLS=(
    "npm_config_git=$(command -v git || echo git)"
    # Never empty: npm skips an empty `npm_config_*` variable and falls back
    # to the tree's file (@npmcli/config `loadEnv`). A space is no option.
    "npm_config_node_options=${NODE_OPTIONS:- }"
    "npm_config_script_shell=/bin/sh"
    "npm_config_shell=/bin/sh"
  )
  # npm's own node-gyp, read where no project config applies.
  gyp=$(cd / && npm config get node-gyp 2>/dev/null)
  case "$gyp" in /*) NPM_TRUSTED_TOOLS+=("npm_config_node_gyp=$gyp") ;; esac
}

# Whether a Yarn root cannot be installed without running a file from the tree.
# Prints the reason when it cannot. The scanner reads every `.yarnrc.yml` Yarn
# would read for the root, as YAML, executing nothing (src/untrusted_yarn.rs).
# A root it cannot answer for is refused, never installed unread.
yarn_untrusted_refusal() {
  local answer problem status
  if [ -z "${CARRICK_CLI:-}" ]; then
    echo "no Carrick was named to read its .yarnrc.yml files (CARRICK_CLI is unset)"
    return 0
  fi
  # stdout is the answer; stderr is only read when there is none, since a
  # wrapper may print notices there.
  problem=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/carrick-yarnrc.XXXXXX") || {
    echo "its .yarnrc.yml files could not be read"
    return 0
  }
  answer=$("$CARRICK_CLI" __untrusted-yarn-refusal "$1" 2>"$problem")
  status=$?
  if [ $status -ne 0 ]; then
    echo "its .yarnrc.yml files could not be read (exit $status): $(tail -n 1 "$problem")"
    rm -f "$problem"
    return 0
  fi
  rm -f "$problem"
  [ -n "$answer" ] || return 1
  echo "$answer"
}

# A failed attempt's log: the first 30 lines (npm puts the cause of a refusal
# there and follows it with usage text), a marker only when lines were cut,
# then the last 20. A log of 50 lines or fewer prints whole.
print_failed_log() {
  local log="$1" total
  [ -f "$log" ] || return 0
  total=$(wc -l <"$log" | tr -d ' ')
  if [ "$total" -le 50 ]; then
    cat "$log"
  else
    head -n 30 "$log"
    echo "... ($((total - 50)) lines omitted) ..."
    tail -n 20 "$log"
  fi
}

# The files a lockfile-tolerant retry may rewrite, per manager.
retry_guarded_files() {
  case "$1" in
    npm) echo "package-lock.json package.json" ;;
    pnpm) echo "pnpm-lock.yaml package.json pnpm-workspace.yaml" ;;
    yarn) echo "yarn.lock package.json" ;;
    bun) echo "bun.lock bun.lockb package.json" ;;
  esac
}

# Every `node_modules` directory under a root, one per line, sorted; `.git` and
# the `node_modules` found are not descended into.
node_modules_dirs() {
  find "$1" -name .git -prune -o -name node_modules -type d -prune -print 2>/dev/null | sort
}

# The retry command for a manager, run through `run_install` like the strict
# one, with every untrusted-mode guard the strict install has.
run_retry_install() {
  local root="$1" log="$2" manager="$3"
  case "$manager" in
    npm)
      if [ -n "$UNTRUSTED" ]; then
        npm_trusted_tools
        run_install "$root" "$log" env "${NPM_TRUSTED_TOOLS[@]}" npm install --ignore-scripts --no-audit --no-fund --no-save
      else
        run_install "$root" "$log" npm install --ignore-scripts --no-audit --no-fund --no-save
      fi
      ;;
    pnpm)
      if [ -n "$UNTRUSTED" ]; then
        run_install "$root" "$log" corepack pnpm install --no-frozen-lockfile --ignore-scripts --ignore-pnpmfile
      else
        run_install "$root" "$log" corepack pnpm install --no-frozen-lockfile --ignore-scripts
      fi
      ;;
    yarn) run_install "$root" "$log" yarn install --pure-lockfile --ignore-scripts ;;
    bun) run_install "$root" "$log" bun install --ignore-scripts --no-save ;;
  esac
}

# Yarn Berry's lockfile opens with a `__metadata` block; classic's does not.
yarn_is_berry() {
  [ -f "$1/.yarnrc.yml" ] || grep -q '^__metadata:' "$1/yarn.lock" 2>/dev/null
}

cmd_install_one() {
  local root="$1" manager="$2" log status refusal strict_cmd="" no_retry="" snap="" before=""
  if [ "$manager" != "deno" ] && { [ -f "$root/deno.json" ] || [ -f "$root/deno.jsonc" ]; }; then
    # Mixed roots need the global Deno cache AND the Node compiler's local
    # install. Neither preparation executes authorized project lifecycle code.
    cmd_install_one "$root" deno
  fi
  log=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/carrick-dependency-install.XXXXXX") || return 1

  # corepack ships with the Node the Action set up; it is how pnpm and yarn
  # reach the version the repo pins in `packageManager`.
  corepack enable >/dev/null 2>&1 || true

  # Taken before the first attempt, so a retry that fails can remove what the
  # attempts created and nothing else.
  if [ -n "$LOCKFILE_RETRY" ]; then
    before=$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/carrick-node-modules.XXXXXX") || before=""
    [ -n "$before" ] && node_modules_dirs "$root" >"$before"
  fi

  case "$manager" in
    deno)
      # No retry: the pre-flight asks about a Deno service only when its config
      # sets `nodeModulesDir`, and no failing `deno install --frozen` has been seen.
      no_retry=1
      strict_cmd="deno install --frozen"
      # A project can authorize lifecycle scripts through allowScripts in its
      # config. Deno has no --ignore-scripts: disabling node_modules is what
      # prevents those scripts from running, even for an authorizing config.
      # Frozen mode also prevents rewriting the project's lockfile.
      run_install "$root" "$log" deno install --frozen --node-modules-dir=none
      status=$?
      ;;
    npm)
      strict_cmd="npm ci"
      if [ -n "$UNTRUSTED" ]; then
        npm_trusted_tools
        run_install "$root" "$log" env "${NPM_TRUSTED_TOOLS[@]}" npm ci --ignore-scripts
      else
        run_install "$root" "$log" npm ci --ignore-scripts
      fi
      status=$?
      ;;
    pnpm)
      strict_cmd="pnpm install --frozen-lockfile"
      if [ -n "$UNTRUSTED" ]; then
        # pnpm loads the tree's `.pnpmfile.cjs` on install unless told not to.
        run_install "$root" "$log" corepack pnpm install --frozen-lockfile --ignore-scripts --ignore-pnpmfile
      else
        run_install "$root" "$log" corepack pnpm install --frozen-lockfile --ignore-scripts
      fi
      status=$?
      ;;
    yarn)
      if [ -n "$UNTRUSTED" ] && refusal=$(yarn_untrusted_refusal "$root"); then
        [ -f "$log" ] && rm -f "$log"
        warn "Carrick did not install $root with yarn: $refusal. The scan will refuse that service rather than run the repository's code or index \`any\` through its packages."
        return 0
      fi
      # Classic Yarn accepts `--mode=skip-build` without skipping the build,
      # so the two lines are told apart by what the tree says they are,
      # never by which command happens to exit 0.
      if yarn_is_berry "$root"; then
        # No retry: Berry has no install mode that reads the lockfile without
        # writing it, a mutable install can write `.pnp.cjs` and `.yarn/cache`
        # that zero-install repos commit, and lifting
        # YARN_ENABLE_IMMUTABLE_INSTALLS undoes the guard from #2246/#2251.
        no_retry=1
        strict_cmd="yarn install --mode=skip-build"
        run_install "$root" "$log" yarn install --mode=skip-build
      elif [ -n "$UNTRUSTED" ]; then
        # Frozen, or classic rewrites yarn.lock.
        strict_cmd="yarn install --frozen-lockfile"
        run_install "$root" "$log" yarn install --frozen-lockfile --ignore-scripts
      else
        strict_cmd="yarn install"
        run_install "$root" "$log" yarn install --ignore-scripts
      fi
      status=$?
      ;;
    bun)
      if ! command -v bun >/dev/null 2>&1; then
        warn "Carrick found a bun lockfile but bun is not installed on this runner; add oven-sh/setup-bun before the Carrick step, or types through dependencies stay \`any\`."
        return 0
      fi
      if [ -n "$UNTRUSTED" ]; then
        strict_cmd="bun install --frozen-lockfile"
        run_install "$root" "$log" bun install --ignore-scripts --frozen-lockfile
      else
        strict_cmd="bun install"
        run_install "$root" "$log" bun install --ignore-scripts
      fi
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
    [ -n "$before" ] && rm -f "$before"
    return 0
  fi

  print_failed_log "$log"

  # One lockfile-tolerant retry (CARRICK_INSTALL_LOCKFILE_RETRY=1). A timeout
  # is not retried: a second attempt would only spend the limit again.
  if [ -n "$LOCKFILE_RETRY" ] && [ -z "$no_retry" ] && [ $status -ne 124 ] && [ $status -ne 137 ]; then
    echo "Carrick's lockfile install ($strict_cmd) in $root failed (exit $status); retrying once without the frozen lockfile, lifecycle scripts still disabled."
    snap=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/carrick-retry-snapshot.XXXXXX") || snap=""
    if [ -z "$snap" ]; then
      warn "Carrick could not snapshot $root before retrying its install; the scan will refuse that service rather than index \`any\` through its packages."
      [ -n "$before" ] && rm -f "$before"
      return 0
    fi
    # The tree stays the commit: put the lockfile and manifests back byte for
    # byte afterwards, whatever the retry did (pnpm rewrites its lockfile).
    local file
    for file in $(retry_guarded_files "$manager"); do
      if [ -e "$root/$file" ]; then cp -p "$root/$file" "$snap/$file"; else : >"$snap/absent.$file"; fi
    done
    run_retry_install "$root" "$log" "$manager"
    status=$?
    for file in $(retry_guarded_files "$manager"); do
      if [ -e "$snap/$file" ]; then cp -p "$snap/$file" "$root/$file"; else rm -f "$root/$file"; fi
    done
    rm -rf "$snap"
    if [ $status -eq 0 ]; then
      echo "Installed $root dependencies with $manager after its lockfile install failed; the lockfile and package.json in the tree are unchanged (lifecycle scripts disabled)."
      [ -f "$log" ] && tail -n 3 "$log"
      [ -n "$before" ] && rm -f "$before"
      return 0
    fi
    print_failed_log "$log"
  fi

  # A root that ends uninstalled keeps no node_modules the attempts created:
  # the pre-flight reads one as prepared, and the scan would read `any` silently.
  if [ -n "$before" ]; then
    local created
    while IFS= read -r created; do
      [ -n "$created" ] && rm -rf "$created"
    done < <(node_modules_dirs "$root" | comm -13 "$before" -)
    rm -f "$before"
  fi
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
  if [ -n "$UNTRUSTED" ]; then
    # Inherited by every install this script starts. Yarn: never hand the
    # install to a release file committed in the tree, never run build
    # scripts, never rewrite the lockfile. corepack: never write
    # `packageManager` into the repo's package.json.
    export YARN_IGNORE_PATH=1 YARN_ENABLE_SCRIPTS=0 YARN_ENABLE_IMMUTABLE_INSTALLS=1 COREPACK_ENABLE_AUTO_PIN=0
  fi
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
