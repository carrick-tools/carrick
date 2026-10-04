#!/usr/bin/env bash
#
# Scan-twice determinism probe (carrick#599).
#
# Runs the scanner twice over the same tree under the mock analyzer and asserts
# the two eval projections are identical, then twice more and asserts the two
# indexes it would upload are the same bytes (carrick#1876). The mock analyzer
# replays a fixture's `__llm__/` cassette (or returns nothing at all where a
# fixture has none), so the model is held constant and any difference between
# the two runs comes from the scanner itself: iteration order over a HashMap, a
# pass that reads a previous pass's leftovers, a span that depends on visit
# order.
#
# The projection is compared with its keys and its top-level arrays sorted, so
# it reads the row set. The blob is compared as written, so it also reads the
# order of every key, list and line of type text. A binary that finds its type
# sidecar writes types into the blob; one that does not writes none, and the
# comparison then covers no type text.
#
# This is a determinism reading, not an accuracy reading. It says nothing about
# whether a row is correct, only that the same input produces the same row set
# twice. Accuracy is the fixture tests and the live eval.
#
# Usage:
#   scripts/scan-twice.sh [bench-mirror-service-dir]
#
# The optional argument is a directory holding a one-service `carrick.json`
# whose service points into a large real-world TypeScript tree. It is scanned
# from inside with `.` as the path, the way the offline probe recipe runs it.
# Nothing about that tree is checked in here.
#
# Environment:
#   CARRICK_BIN   scanner binary (default: target/debug/carrick)
#   SCAN_OUT_DIR  where to keep the two projections and the two blob
#                 directories per target (default: a fresh mktemp dir, printed
#                 on the first line)
#
# Exit status is non-zero if any target differs between its two runs.

set -uo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"

bin="${CARRICK_BIN:-$repo_root/target/debug/carrick}"
if [ ! -x "$bin" ]; then
  echo "no scanner binary at $bin (cargo build first, or set CARRICK_BIN)" >&2
  exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required for the JSON canonicalisation" >&2
  exit 2
fi

out_dir="${SCAN_OUT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/carrick-scan-twice.XXXXXX")}"
mkdir -p "$out_dir"
echo "scan-twice: binary $bin"
echo "scan-twice: output  $out_dir"

# --- targets -----------------------------------------------------------------
#
# Rule: every fixture under tests/fixtures that carries a `carrick.json` or is
# driven by an existing tests/*_test.rs, listed explicitly so the probe's scope
# is reviewable rather than derived from a grep over test sources. Multi-repo
# fixtures are listed per repo, because each repo carries its own cassette and
# the scanner is pointed at one repo at a time.
#
# `e2e-scaffolding` matches neither half of the rule; it is here because
# carrick#599 names it. `extraction-config` matches the rule but is absent: it
# holds a cassette and no source at all, its test reads that JSON directly, and
# the scanner refuses a tree with no TS/JS in it. There is nothing to scan
# twice. `deno-lock` and `library-store-deno` are absent for the same reason:
# they hold lockfiles only, which their tests lay over a tree of their own.
fixture_targets=(
  astro
  class-controller-api
  client-semantics
  demo-services-shape
  e2e-scaffolding
  env-var-whole-url
  fastify-api
  file-route-model-twin
  flat-routes-declared-method
  flat-routes-method-guard
  graphql-service
  imported-request-member
  imported-routers
  koa-api
  library-store
  literal-base-url
  llm-mocked-api
  new-url-target
  nextjs-app
  nextjs-app-monorepo
  payload-without-method
  pubsub-wrapper-monorepo
  remix-flat
  request-summary
  scenario-1-dependency-conflicts
  scenario-3-cross-repo-success/repo-a
  scenario-3-cross-repo-success/repo-b
  scenario-3-cross-repo-success/repo-c
  socket-class-field-monorepo
  socket-namespace-monorepo
  socket-service
  socket-type-alias-monorepo
  spread-options-verb
  workspace-package-client
  xrepo-corpus-3
)

# The fixtures carrick#599 requires the probe to cover. Missing one is a bug in
# the list above, not a scanner finding.
required=(
  env-var-whole-url
  new-url-target
  imported-request-member
  literal-base-url
  class-controller-api
  flat-routes-method-guard
  e2e-scaffolding
)
for want in "${required[@]}"; do
  found=0
  for have in "${fixture_targets[@]}"; do
    [ "$have" = "$want" ] && found=1
  done
  if [ "$found" -ne 1 ]; then
    echo "target list is missing the required fixture $want" >&2
    exit 2
  fi
done

mirror_dir="${1:-}"
if [ -n "$mirror_dir" ] && [ ! -d "$mirror_dir" ]; then
  echo "bench mirror service dir does not exist: $mirror_dir" >&2
  exit 2
fi

# --- one scan ----------------------------------------------------------------
#
# `--no-cache` for a full analysis both times; the mock analyzer for a constant
# model; no intents, because intent generation is a separate lambda and is not
# part of the projection. CI-shaped env vars are removed so the run takes the
# same branch on a developer machine and on a runner.
#
# With a sixth argument the scan writes the index it would upload into that
# directory instead of printing a projection (carrick#1876). The two outputs
# need a scan each: a run that prints the projection uploads nothing. The
# directory is read back as empty, so the scan is a cold one with no sibling.
scan() {
  scan_dir="$1"
  scan_path="$2"
  mock_dir="$3"
  stdout_file="$4"
  stderr_file="$5"
  blob_dir="${6:-}"

  unset_vars=(-u GITHUB_REPOSITORY -u GITHUB_ACTIONS -u CI)
  # A fixture that states dependencies and has none installed is scanned as it
  # is: the scanner otherwise refuses it at preflight, and a refused target is
  # not probed at all.
  set_vars=(CARRICK_MOCK_ALL=1 CARRICK_SKIP_INTENTS=1 CARRICK_ALLOW_UNPREPARED=1)
  if [ -n "$mock_dir" ]; then
    set_vars+=("CARRICK_MOCK_FIXTURE_DIR=$mock_dir")
  else
    unset_vars+=(-u CARRICK_MOCK_FIXTURE_DIR)
  fi
  if [ -n "$blob_dir" ]; then
    unset_vars+=(-u CARRICK_OUTPUT_JSON -u GITHUB_EVENT_NAME -u GITHUB_REF)
    set_vars+=("CARRICK_LOCAL_STORAGE_DIR=$blob_dir" CARRICK_LOCAL_STORAGE_ISOLATE=1)
  else
    set_vars+=(CARRICK_OUTPUT_JSON=1)
  fi

  (
    cd "$scan_dir" || exit 3
    env "${unset_vars[@]}" "${set_vars[@]}" "$bin" "$scan_path" --no-cache
  ) >"$stdout_file" 2>"$stderr_file"
}

# --- canonical comparison ----------------------------------------------------
#
# Object keys are sorted at every depth. The four top-level arrays (endpoints,
# calls, cross_repo_matches, dependency_conflicts) are sorted by the canonical
# serialisation of each element, which is a stable key that needs no knowledge
# of the row shape and survives several rows sharing a (file, line). Arrays
# nested inside a row are deliberately NOT sorted: if one of those reorders
# between two runs, that is a real finding and the probe should say so.
compare_py="$out_dir/_compare.py"
cat >"$compare_py" <<'PY'
import json
import sys

TOP_ARRAYS = ("endpoints", "calls", "cross_repo_matches", "dependency_conflicts")


def ser(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)


def canonical(doc):
    if not isinstance(doc, dict):
        return doc
    out = {}
    for key, value in doc.items():
        if key in TOP_ARRAYS and isinstance(value, list):
            out[key] = sorted(value, key=ser)
        else:
            out[key] = value
    return out


def clip(text, limit=400):
    return text if len(text) <= limit else text[:limit] + "…"


def first_diff(a, b, path="$"):
    if isinstance(a, dict) and isinstance(b, dict):
        for key in sorted(set(a) | set(b)):
            here = f"{path}.{key}"
            if key not in a:
                return here, "<absent in run 1>", ser(b[key])
            if key not in b:
                return here, ser(a[key]), "<absent in run 2>"
            found = first_diff(a[key], b[key], here)
            if found:
                return found
        return None
    if isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            sa = [ser(x) for x in a]
            sb = [ser(x) for x in b]
            only_a = [s for s in sa if s not in sb]
            only_b = [s for s in sb if s not in sa]
            return (
                f"{path}[] length {len(a)} vs {len(b)}",
                only_a[0] if only_a else "<no row unique to run 1>",
                only_b[0] if only_b else "<no row unique to run 2>",
            )
        for index, (x, y) in enumerate(zip(a, b)):
            found = first_diff(x, y, f"{path}[{index}]")
            if found:
                return found
        return None
    if type(a) is not type(b) or a != b:
        return path, ser(a), ser(b)
    return None


def load(path):
    with open(path, "r", encoding="utf-8") as handle:
        text = handle.read()
    try:
        return json.loads(text)
    except json.JSONDecodeError as err:
        print(f"not JSON: {path}: {err}")
        raise SystemExit(2)


def check_shape(doc, path):
    # A projection that lost its arrays would otherwise compare equal to
    # another projection that lost them the same way, and read as PASS.
    if not isinstance(doc, dict):
        print(f"not a projection object: {path}")
        raise SystemExit(2)
    missing = [key for key in TOP_ARRAYS if key not in doc]
    if missing:
        print(f"projection is missing {', '.join(missing)}: {path}")
        raise SystemExit(2)


one, two = load(sys.argv[1]), load(sys.argv[2])
check_shape(one, sys.argv[1])
check_shape(two, sys.argv[2])
diff = first_diff(canonical(one), canonical(two))
if diff is None:
    raise SystemExit(0)
path, left, right = diff
print(f"first differing path: {path}")
print(f"  run 1: {clip(left)}")
print(f"  run 2: {clip(right)}")
raise SystemExit(1)
PY

# --- blob comparison ---------------------------------------------------------
#
# The index a scan uploads, compared as written (carrick#1876). Nothing is
# sorted here: whatever stores a blob compares its bytes, so a key or a list in
# another order is a difference. `last_updated` states when the scan ran and is
# the one field left out. The first differing path of each blob is printed,
# with what kind of difference it is.
blob_compare_py="$out_dir/_compare_blobs.py"
cat >"$blob_compare_py" <<'PY'
import json
import sys
from pathlib import Path


def load(path):
    # Objects as key/value lists, so the order the keys were written in is kept.
    pairs = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=lambda p: ("object", p))
    return ("object", [(k, v) for k, v in pairs[1] if k != "last_updated"])


def is_object(value):
    return isinstance(value, tuple) and len(value) == 2 and value[0] == "object"


def first_diff(a, b, path="$"):
    if is_object(a) and is_object(b):
        keys_a = [k for k, _ in a[1]]
        keys_b = [k for k, _ in b[1]]
        if keys_a != keys_b:
            if sorted(keys_a) == sorted(keys_b):
                return path, "the same keys in another order"
            return path, "different keys"
        for (key, x), (_, y) in zip(a[1], b[1]):
            found = first_diff(x, y, f"{path}.{key}")
            if found:
                return found
        return None
    if isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            return path, f"{len(a)} elements in run 1, {len(b)} in run 2"
        for index, (x, y) in enumerate(zip(a, b)):
            found = first_diff(x, y, f"{path}[{index}]")
            if found:
                return found
        return None
    if type(a) is not type(b) or a != b:
        if isinstance(a, str) and isinstance(b, str) and sorted(a.split("\n")) == sorted(b.split("\n")):
            return path, "the same lines in another order"
        return path, "different values"
    return None


one, two = Path(sys.argv[1]), Path(sys.argv[2])
names = sorted({p.name for p in one.glob("*.json")} | {p.name for p in two.glob("*.json")})
if not names:
    # No blob from either run is not a PASS: the scan uploaded nothing.
    print("neither scan wrote a blob")
    raise SystemExit(2)
differing = 0
for name in names:
    if not (one / name).exists() or not (two / name).exists():
        print(f"{name}: written by one scan only")
        differing += 1
        continue
    diff = first_diff(load(one / name), load(two / name))
    if diff:
        print(f"{name}: first differing path {diff[0]} ({diff[1]})")
        differing += 1
raise SystemExit(1 if differing else 0)
PY

# --- run ---------------------------------------------------------------------
failures=0
run_target() {
  label="$1"
  scan_dir="$2"
  scan_path="$3"
  mock_dir="$4"

  slug="$(printf '%s' "$label" | tr '/' '_')"
  a_out="$out_dir/$slug.1.json"
  b_out="$out_dir/$slug.2.json"
  a_err="$out_dir/$slug.1.err"
  b_err="$out_dir/$slug.2.err"
  a_blobs="$out_dir/$slug.blobs.1"
  b_blobs="$out_dir/$slug.blobs.2"
  rm -rf "${a_blobs:?}" "${b_blobs:?}"

  started="$(date +%s)"
  scan "$scan_dir" "$scan_path" "$mock_dir" "$a_out" "$a_err"
  a_status=$?
  scan "$scan_dir" "$scan_path" "$mock_dir" "$b_out" "$b_err"
  b_status=$?
  scan "$scan_dir" "$scan_path" "$mock_dir" "$out_dir/$slug.blobs.1.out" "$out_dir/$slug.blobs.1.err" "$a_blobs"
  a_blob_status=$?
  scan "$scan_dir" "$scan_path" "$mock_dir" "$out_dir/$slug.blobs.2.out" "$out_dir/$slug.blobs.2.err" "$b_blobs"
  b_blob_status=$?
  elapsed=$(( $(date +%s) - started ))

  if [ "$a_status" -ne 0 ] || [ "$b_status" -ne 0 ]; then
    printf 'FAIL  %-44s %4ss  scanner exited %s / %s\n' "$label" "$elapsed" "$a_status" "$b_status"
    tail -n 5 "$a_err" | sed 's/^/        /'
    failures=$((failures + 1))
    return
  fi
  if [ "$a_blob_status" -ne 0 ] || [ "$b_blob_status" -ne 0 ]; then
    printf 'FAIL  %-44s %4ss  the blob scan exited %s / %s\n' "$label" "$elapsed" "$a_blob_status" "$b_blob_status"
    tail -n 5 "$out_dir/$slug.blobs.1.err" | sed 's/^/        /'
    failures=$((failures + 1))
    return
  fi

  # Always compare through the canonicaliser, even when the two files are
  # byte-identical: it is what proves each run emitted a projection at all.
  # Two identical empty stdouts are not a determinism PASS, they are a scanner
  # that stopped emitting.
  detail="$(python3 "$compare_py" "$a_out" "$b_out")"
  compare_status=$?
  blob_detail="$(python3 "$blob_compare_py" "$a_blobs" "$b_blobs")"
  blob_status=$?

  if [ "$compare_status" -eq 0 ] && [ "$blob_status" -eq 0 ]; then
    if cmp -s "$a_out" "$b_out"; then
      printf 'PASS  %-44s %4ss  projection and blob byte-identical\n' "$label" "$elapsed"
    else
      # The canonical forms agree, so the row set is the same and only the
      # order it was emitted in moved. Not a failure today; worth watching,
      # because an emission-order change can turn it into one.
      printf 'PASS  %-44s %4ss  blob byte-identical, order-only diff in the raw projection\n' "$label" "$elapsed"
    fi
    return
  fi

  printf 'FAIL  %-44s %4ss\n' "$label" "$elapsed"
  if [ "$compare_status" -ne 0 ]; then
    printf '%s\n' "$detail" | sed 's/^/        projection: /'
  fi
  if [ "$blob_status" -ne 0 ]; then
    printf '%s\n' "$blob_detail" | sed 's/^/        blob: /'
  fi
  failures=$((failures + 1))
}

for fixture in "${fixture_targets[@]}"; do
  dir="$repo_root/tests/fixtures/$fixture"
  if [ ! -d "$dir" ]; then
    printf 'FAIL  %-44s     -  no such fixture directory\n' "$fixture"
    failures=$((failures + 1))
    continue
  fi
  mock=""
  [ -d "$dir/__llm__" ] && mock="$dir/__llm__/"
  run_target "$fixture" "$repo_root" "$dir" "$mock"
done

if [ -n "$mirror_dir" ]; then
  mirror_abs="$(cd "$mirror_dir" && pwd)"
  mock=""
  [ -d "$mirror_abs/__llm__" ] && mock="$mirror_abs/__llm__/"
  run_target "bench-mirror" "$mirror_abs" "." "$mock"
fi

echo
if [ "$failures" -eq 0 ]; then
  echo "scan-twice: every target is deterministic across two scans"
  exit 0
fi
echo "scan-twice: $failures target(s) differed between two scans"
exit 1
