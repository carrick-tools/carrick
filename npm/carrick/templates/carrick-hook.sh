#!/bin/sh
# Carrick's agent-hook launcher (carrick#4759). Written by `carrick init`, beside
# the settings file that calls it; init rewrites it, so do not edit it.
#
#   carrick-hook.sh hook <event> [...]   run `carrick hook <event>`
#   carrick-hook.sh --where              print the carrick it would run
#
# A hook entry that calls a bare `carrick` fails with `command not found` when
# the default Node changes, because a global install lives under the Node that
# installed it. This looks for the install the way a shell would not:
#
#   1. `carrick` on PATH.
#   2. The places a global install lands under nvm, fnm, volta, asdf, mise and
#      the npm global prefix, and this project's node_modules/.bin. The newest
#      Node's install wins. It is started with its own bin directory first on
#      PATH, so `#!/usr/bin/env node` picks the Node it was installed under.
#   3. Nothing found: print one line at session start, nothing for any other
#      event, and exit 0, so the agent is never blocked.
#
# File tests only: no `which`, no `npm`, no subprocess. It runs before every
# edit, and the lookup is skipped whenever `carrick` is on PATH.

if command -v carrick >/dev/null 2>&1; then
  if [ "${1-}" = "--where" ]; then
    command -v carrick
    exit 0
  fi
  exec carrick "$@"
fi

# Whether dotted version $1 is newer than $2, compared as numbers (v9 < v24).
ver_gt() {
  a=$1
  b=$2
  i=0
  while [ "$i" -lt 3 ]; do
    x=${a%%.*}
    y=${b%%.*}
    case $x in '' | *[!0-9]*) x=0 ;; esac
    case $y in '' | *[!0-9]*) y=0 ;; esac
    [ "$x" -gt "$y" ] && return 0
    [ "$x" -lt "$y" ] && return 1
    case $a in *.*) a=${a#*.} ;; *) a=0 ;; esac
    case $b in *.*) b=${b#*.} ;; *) b=0 ;; esac
    i=$((i + 1))
  done
  return 1
}

best=''
best_ver=''

# One candidate binary under a version label. The newest label wins.
consider() {
  [ -f "$1" ] && [ -x "$1" ] || return 0
  if [ -z "$best" ] || ver_gt "$2" "$best_ver"; then
    best=$1
    best_ver=$2
  fi
}

# Every <root>/<version>/<between>/carrick.
versioned() {
  for bin in "$1"/*/"$2"/carrick; do
    v=${bin#"$1"/}
    v=${v%%/*}
    consider "$bin" "${v#v}"
  done
}

home=${HOME-}

versioned "${NVM_DIR:-$home/.nvm}/versions/node" bin
versioned "${FNM_DIR:-$home/.local/share/fnm}/node-versions" installation/bin
versioned "${XDG_DATA_HOME:-$home/.local/share}/fnm/node-versions" installation/bin
versioned "$home/Library/Application Support/fnm/node-versions" installation/bin
versioned "${ASDF_DATA_DIR:-$home/.asdf}/installs/nodejs" bin
versioned "${MISE_DATA_DIR:-$home/.local/share/mise}/installs/node" bin

# Not versioned by directory: a global bin directory of its own. Ranked below
# any versioned install, in the order written.
consider "${VOLTA_HOME:-$home/.volta}/bin/carrick" 0.0.2
consider "${CLAUDE_PROJECT_DIR:-$PWD}/node_modules/.bin/carrick" 0.0.1

npmrc_prefix=''
if [ -f "$home/.npmrc" ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    case $line in
      prefix=*)
        npmrc_prefix=${line#prefix=}
        case $npmrc_prefix in "~"*) npmrc_prefix=$home${npmrc_prefix#"~"} ;; esac
        ;;
    esac
  done <"$home/.npmrc"
fi
for prefix in "${npm_config_prefix-}" "${NPM_CONFIG_PREFIX-}" "$npmrc_prefix" "$home/.npm-global" /usr/local /opt/homebrew "$home/.local"; do
  [ -n "$prefix" ] && consider "$prefix/bin/carrick" 0.0.0
done

if [ "${1-}" = "--where" ]; then
  [ -n "$best" ] && printf '%s\n' "$best"
  exit 0
fi

if [ -n "$best" ]; then
  PATH="${best%/*}:$PATH"
  export PATH
  exec "$best" "$@"
fi

# Not found. Only the session start speaks; Claude Code takes the line as
# printed, Codex takes it as the JSON its parser reads (the rule
# `runByCodex` in src/hook/session-start.ts uses: no CLAUDE_PROJECT_DIR).
if [ "${1-}" = "hook" ] && [ "${2-}" = "session-start" ]; then
  message='Carrick is not installed in this shell; agent hooks are off. Run `npm install -g carrick`, or switch to the Node version where it is installed.'
  if [ -n "${CLAUDE_PROJECT_DIR-}" ]; then
    printf '%s\n' "$message"
  else
    printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"%s"}}\n' "$message"
  fi
fi
exit 0
