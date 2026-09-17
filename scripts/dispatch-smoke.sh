#!/usr/bin/env bash
#
# The cold-cache dispatch smoke (carrick#1259).
#
# carrick#1257 — the scanner never sent `scan_id`, so the cloud refused every
# dispatch with a 400 — shipped with both repos' CI green and survived three
# manual test attempts. It survived because `index --dispatch` only submits
# when there is something to hand over: against a warm analysis cache the scan
# takes the synchronous fallback and never reaches the submit code
# (carrick#1251). Green on both sides proves nothing about the join, and a
# dispatch test over a warm cache proves nothing about the dispatch.
#
# So this script forces the cache cold — the analysis cache is keyed on file
# CONTENT (carrick#1233), so a line unique to this run appended to every source
# file is what re-asks — hands the prompts over for real, waits for the cloud
# driver, and collects them.
#
# It spends money: one run is a live analysis of a six-file fixture, a few
# cents of inference. It is not part of the per-PR suite; see
# .github/workflows/dispatch-smoke.yml for the cadence.
#
# Required environment:
#
#   CARRICK_TOKEN        A `cli`-scope credential, as `carrick login` writes to
#                        ~/.config/carrick/credentials.json. NOT OIDC: a
#                        GitHub Actions identity opens no scan, so it has no
#                        scan_id to submit with, and `status`/`resume` read the
#                        job with a bearer token and nothing else.
#   CARRICK_SMOKE_REPO   `owner/repo` the token's workspace authorises. The
#                        fixture is given this as its git remote, which is
#                        where the scan's repo identity comes from, so the
#                        index this writes lands on that repo's row. Point it
#                        at a repo that exists for this and nothing else.
#
# Optional:
#
#   CARRICK_BIN              the scanner (default target/release/carrick)
#   CARRICK_API_ENDPOINT     where THIS SCRIPT's own job-status reads go
#                            (default https://api.carrick.tools). Not the
#                            scanner's: its endpoint is fixed when it is built,
#                            and the job reads inside it use a const. Pointing
#                            this at a second cloud would ask one about a job
#                            the other holds.
#   CARRICK_SMOKE_TIMEOUT    seconds to wait for the driver (default 900)
#
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fixture="$repo_root/tests/fixtures/llm-mocked-api"
carrick=${CARRICK_BIN:-$repo_root/target/release/carrick}
api=${CARRICK_API_ENDPOINT:-https://api.carrick.tools}
timeout=${CARRICK_SMOKE_TIMEOUT:-900}
# How often the driver is asked. A six-file job finishes in about a minute, so
# this is a handful of reads, not a poll storm.
interval=10

fail() {
  echo "dispatch-smoke: $*" >&2
  exit 1
}

step() { echo "dispatch-smoke: $*"; }

[ -n "${CARRICK_TOKEN:-}" ] || fail "CARRICK_TOKEN is not set. It needs a cli-scope credential."
[ -n "${CARRICK_SMOKE_REPO:-}" ] || fail "CARRICK_SMOKE_REPO is not set (owner/repo)."
[ -x "$carrick" ] || fail "no scanner at $carrick. Build it, or set CARRICK_BIN."
command -v jq >/dev/null || fail "jq is not installed."

# CloudAuth picks OIDC whenever the runner offers it, BEFORE it reads
# CARRICK_TOKEN (src/credentials.rs). An OIDC run opens no scan, so its
# dispatch carries an empty scan_id and the cloud refuses it — the shape of
# the very bug this guards. A smoke that failed that way would read as the bug
# rather than as its own misconfiguration, so refuse up front.
if [ -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ]; then
  fail "this run offers an OIDC token, which the scanner would use instead of \
CARRICK_TOKEN. Drop 'id-token: write' from the job's permissions."
fi

api_post() {
  local body=$1 out status
  out=$(mktemp)
  status=$(curl -sS --max-time 30 -o "$out" -w '%{http_code}' \
    -H "Authorization: Bearer $CARRICK_TOKEN" \
    -H 'content-type: application/json' \
    -d "$body" "$api/types/check-or-upload" || echo 000)
  if [ "$status" != "200" ]; then
    echo "HTTP $status: $(head -c 400 "$out")" >&2
    rm -f "$out"
    return 1
  fi
  cat "$out"
  rm -f "$out"
}

work=$(mktemp -d)
tree="$work/repo"
index_dir="$tree/.carrick"
jobs_file="$index_dir/jobs.json"
collected=0

cleanup() {
  # A dispatched run holds the cloud's in-flight slot for this repo until a
  # resume writes the index. A smoke that dies in between would leave the slot
  # held and refuse its own next run, so give the job back.
  if [ "$collected" -eq 0 ] && [ -f "$jobs_file" ]; then
    local job
    job=$(jq -r '.jobs[0].job_id // empty' "$jobs_file" 2>/dev/null || true)
    if [ -n "$job" ]; then
      step "cancelling job $job so the next run is not refused"
      api_post "{\"action\":\"cancel-analysis-job\",\"job_id\":\"$job\"}" >/dev/null || true
    fi
  fi
  rm -rf "$work"
}
trap cleanup EXIT

# 1. The fixture, in a tree of its own. Copied rather than mutated in place:
#    the repo's own checkout is not the thing being scanned, and its identity
#    is not the one the scan should upload under.
step "staging $fixture"
mkdir -p "$tree"
cp -R "$fixture/." "$tree/"
# Cassettes and the golden file belong to the mocked tests. This run asks the
# real model.
rm -rf "$tree/__llm__" "$tree/__golden__.json"

# 2. Cold cache. The analysis cache is keyed on the bytes of each file, so a
#    line no previous run wrote is what makes the cloud analyse rather than
#    answer from cache. Unique per run, and per attempt within a run.
stamp="${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}-$(date -u +%Y%m%dT%H%M%SZ)"
sources=$(find "$tree" -name '*.ts' -type f | sort)
[ -n "$sources" ] || fail "the fixture has no TypeScript files"
while IFS= read -r file; do
  printf '\n// carrick dispatch smoke %s\n' "$stamp" >>"$file"
done <<<"$sources"
step "$(printf '%s\n' "$sources" | wc -l | tr -d ' ') source file(s) made unique for this run"

# 3. Committed, not left dirty. A scan of a tree with uncommitted changes takes
#    the hosted-retention path (carrick#1255) and answers about the previous
#    index instead of this one; the cache is cold either way, because it is
#    keyed on content and not on the commit.
git -C "$tree" init -q
git -C "$tree" add -A
git -C "$tree" -c user.email=smoke@carrick.tools -c user.name='Carrick smoke' \
  commit -qm "dispatch smoke $stamp"
git -C "$tree" remote add origin "https://github.com/$CARRICK_SMOKE_REPO.git"

# 4. Hand it over.
step "carrick index --dispatch"
log="$work/dispatch.log"
if ! "$carrick" index --dispatch --workspace "$tree" 2>&1 | tee "$log"; then
  fail "index --dispatch exited non-zero"
fi

# 5. The load-bearing assertion. `.carrick/jobs.json` is written only from a
#    submission the cloud accepted and named, so its presence is the proof the
#    seam took the job — not that the command exited 0, which it also does when
#    it quietly runs a synchronous scan instead.
[ -f "$jobs_file" ] || fail "nothing was dispatched: no $jobs_file was written. A warm \
cache (carrick#1251) and a refused submission both look like this; the scan's own output \
is above."
schema=$(jq -r '.schema // empty' "$jobs_file")
[ "$schema" = "carrick.jobs/0" ] || fail "jobs.json carries schema '$schema'"
[ "$(jq '.jobs | length' "$jobs_file")" = "1" ] || fail "expected exactly one job"
job_id=$(jq -r '.jobs[0].job_id // empty' "$jobs_file")
rows=$(jq -r '.jobs[0].analyze_rows // 0' "$jobs_file")
job_repo=$(jq -r '.jobs[0].repo // empty' "$jobs_file")
[ -n "$job_id" ] || fail "the recorded job has no id"
[ "$rows" -ge 1 ] || fail "the job was submitted with $rows rows: nothing was handed over"
[ "$job_repo" = "$CARRICK_SMOKE_REPO" ] || fail "the job names repo '$job_repo', not $CARRICK_SMOKE_REPO"
[ ! -f "$index_dir/index.json" ] || fail "a dispatched build wrote an index; it has nothing to write one from"
grep -q 'Carrick Cloud is analysing' "$log" || fail "the hand-off was not reported on stdout"
step "job $job_id took $rows row(s) for $job_repo"

# 6. Wait for the driver. `total_rows` is 0 until it reads the bundle, so the
#    first reads say 0% and that is correct rather than a stall.
waited=0
state=""
answered=0
total=0
told_status=0
while [ "$waited" -lt "$timeout" ]; do
  status=$(api_post "{\"action\":\"analysis-job-status\",\"job_id\":\"$job_id\"}") ||
    fail "could not read the job status"
  state=$(echo "$status" | jq -r '.state // empty')
  answered=$(echo "$status" | jq -r '.answered // 0')
  total=$(echo "$status" | jq -r '.total_rows // 0')
  step "  $state — $answered/$total after ${waited}s"
  case "$state" in
  ready | partial | failed | cancelled) break ;;
  queued | running) ;;
  *) fail "the cloud answered with state '$state'" ;;
  esac
  # Once, while the job is live: the CLI's own read of it. Asserted for
  # content, not parsed — `carrick status` is prose by design.
  if [ "$told_status" -eq 0 ]; then
    told_status=1
    "$carrick" status --workspace "$tree" >"$work/status.txt" 2>&1 || true
    grep -q "$job_repo" "$work/status.txt" ||
      fail "carrick status said nothing about the job it is waiting on: $(cat "$work/status.txt")"
  fi
  sleep "$interval"
  waited=$((waited + interval))
done

[ "$state" = "ready" ] || fail "the job ended '$state' after ${waited}s ($answered/$total answered)"
[ "$total" -ge 1 ] || fail "the job finished holding no rows"
[ "$answered" = "$total" ] || fail "the job is ready with $answered of $total answered"
step "the driver answered $answered/$total"

# 7. Collect. The join is on content, and the tree still holds the bytes the
#    prompts were built from, so every answer should match and nothing should
#    go back to the model.
step "carrick resume"
"$carrick" resume --workspace "$tree" 2>&1 | tee "$work/resume.log" || fail "resume exited non-zero"
[ -f "$index_dir/index.json" ] || fail "resume wrote no index"
[ ! -f "$jobs_file" ] || fail "resume left the collected job recorded"
collected=1

step "OK — dispatched $rows row(s), collected $answered, index written"
