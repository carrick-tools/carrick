# Dispatch and resume

How a scan hands its analysis to Carrick Cloud and how another run collects
it. Pointed at from `src/analysis_job.rs` and `src/analysis_channel.rs`.

## Why

A first index of a large monorepo is thousands of model calls, and every one of
them has to survive on the machine that started the scan. A closed laptop, a
dropped connection or a shell that times out loses the run — and the next
attempt starts again.

`carrick index --dispatch` splits the scan in two. The laptop does everything
up to and including building the prompts, ships them as one object, and exits.
The cloud answers them on its own time. `carrick resume` — on that machine or
another one holding the same repository — replays the answers through an
ordinary scan.

## What the laptop still does

Everything except the model calls for files. Framework detection and guidance
(about seven calls) are synchronous and happen before the hand-off, because the
prompt embeds the rendered guidance block. The deterministic layer runs in
full, including one type-sidecar request per service, because the prompt also
embeds the candidates that layer produces. So the prompts can only be built
here: the cloud has neither the tree, the sidecar, nor the Rust passes.

A dispatched run stops after that. It generates no intents, resolves no types,
writes no index and uploads nothing.

## The two objects

Newline-delimited JSON, gzipped. Compression is a requirement rather than an
economy: the raw stream on a large monorepo is hundreds of megabytes against a
hard per-object ceiling.

```
header  { schema: "carrick.analysis-job/0", scan_id, repo, commit,
          scanner_version, cache_version, services: [...],
          guidance: { <guidance_key>: <rendered block, VERBATIM> },
          schemas:  { <schema_sha>:   <response schema value> },
          counts: { analyze_file, intent_functions, intent_levels } }
row     { t: "analyze" | "intent", ... }
analyze { t: "analyze", id, service, guidance_key, body, schema_sha }
```

The answers come back as **one object per driver pass, with no header line**
(carrick-cloud#1006): a pass writes what it has before it hands on, so nothing
holds the whole job at once and a header written before the last pass would
state a total that was not true yet.

```
answer  { t: "answer",  id, text, cached, truncated }
failure { t: "failure", id, code }
```

What the parts amount to is the `analysis-job-answers` response, not a line in
a file: `{ schema, state, complete, total_rows, answered, parts: [{ part, url,
bytes, rows }], superseded, current_index }`. `carrick resume` folds every part
into one file for the scan that replays them.

## The three actions, as deployed

`submit-analysis-job` is sent twice, the shape payload staging already uses
(carrick#486), because the object is too large to inline and a presigned PUT
carries no integrity condition:

1. `{ action, repo, commit, scanner_version, cache_version, counts,
   wants_upload_url: true }` — the cloud **mints the job id** and answers
   `{ schema, job_id, upload_url, max_bytes }`;
2. PUT the gzipped object at exactly that URL. The cloud heads the key it
   derived itself, so anywhere else is `409 analysis_bundle_missing`;
3. `{ action, …, job_id, payload_sha256, payload_size }` — hex sha256 and byte
   length of the **compressed** object, each validated with its own 400. The
   200 carries `state`, which is `failed` when the driver could not be started.

A second bundle for a repo whose job is still open is `409
analysis_job_in_flight` with the job id in the body. That is not a fault: the
move is `carrick status`, not another dispatch, and the scanner re-states it
rather than reporting a failed scan.

`analysis-job-status` takes `{ job_id }` **or** `{ repo }` — the repo form
follows the cloud's repo -> job pointer — and answers `{ job_id, repo, commit,
state, total_rows, answered, percent, failure_reason, created_at, expires_at }`.
States are `queued`, `running`, `ready`, `partial`, `failed`, `cancelled`; the
last four are terminal, and `partial` is worth collecting. A repo it holds no
job for is a `404`, which is an answer and not a fault. There is **no ETA on
the wire**, so nothing prints one. `analysis-job-answers` takes `{ job_id }`
only, which is why the repo form of the status read is the recovery path for a
lost record: it names the job the answers call needs.

`body` is the prompt AFTER the guidance prefix, byte-exact. The guidance block
and the response schema are carried once each in the header and re-attached by
the driver, which is most of the bytes on a repo with thousands of files. They
are the only two things deduplicated: both are free of byte-exactness risk,
because the cache key does not read the prefix and canonicalises the schema.

The file's path lives inside `body` and is repo-relative (carrick#1223), which
is what lets a bundle be resumed on a different machine at a different path.

## The identity: `id = sha256(body)`

A row's `id` is the hex sha256 of the body bytes and nothing else — the same
slice the cloud's `analysisCacheKeyWithMode` hashes for its own key. Two
consequences, and both are load-bearing:

- **`scanner id == cloud bodySha`** for the same bytes, so an answer already
  comes back under the name the resume will ask for. A test in
  `src/analysis_job.rs` pins it against a digest computed by the cloud's own
  `analysis_cache.js` over a shared fixture; the cloud repo holds the other
  half of that fixture.
- **The guidance prefix is not key material.** A release that rewords the
  guidance renderer does not orphan a dispatched job, and the driver may
  prepend a block of any length provided it sets `guidance_bytes` to what it
  actually prepended.

It is deliberately NOT `prompt_fingerprint`, which covers the whole assembled
message including that prefix.

## The join is content, not a path or a commit

The resume rebuilds each file's prompt locally — cheap, no model — and takes
the answer whose id matches the body it just built. Everything else goes to the
model in the ordinary way: a file edited since the hand-off, an answer the job
could not produce, an answer the model truncated (carrick#692).

That is why the join is not the incremental path's commit diff. A commit diff
falls back to a full scan on a shallow clone or an unreachable previous commit
(carrick#1086) and says nothing at all about a dirty tree — and a resume has to
work in exactly those cases.

A collected answer enters phase 3 as the model's answer, is cached as one, and
is uploaded as one. That is what makes a resume a replay rather than a second
pipeline.

## Where each part lives

| Part | File |
|---|---|
| Wire objects, `body_id`, gzip, the answer reader | `src/analysis_job.rs` |
| Dispatch collector, resume answers, the two env vars | `src/analysis_channel.rs` |
| The one point that asks, collects or replays | `src/agents/file_orchestrator.rs` PHASE 2 |
| The cut, and building and submitting the bundle | `src/engine/mod.rs` (`dispatch_analysis_job`) |
| `submit-analysis-job`, and the capability flag | `src/cloud_storage/aws_storage.rs` |
| The job record, and the two network reads | `src/local_mode/jobs.rs` |
| `--dispatch`, `resume`, the `status` line | `src/local_mode/cli.rs` |

## What each answer means

| The run finds | What happens |
|---|---|
| The cloud does not offer analysis jobs | The scan runs synchronously, and the build says which repo was not handed over and why. A scanner that can dispatch in front of a cloud that cannot is an ordinary scan, not a broken install — but a `--dispatch` that hands nothing over and says nothing reads as a flag that did nothing (carrick#1251) |
| A service whose guidance carries no id | The job is refused rather than sent: without the id the cloud keys the whole message and the block carried once would be paid for once per file |
| Nothing for the model | No job. The repo is indexed here, in the seconds it takes to state facts nobody has to be asked about — and the build says so, naming the repo. This is the ordinary outcome of every `--dispatch` after the first scan, so it is the line `--dispatch` prints most often (carrick#1251) |
| A job still running | `carrick status` and `carrick resume` say how far it has got. Nothing is scanned |
| No `.carrick/jobs.json`, and the cloud holds a job | `carrick resume` asks `analysis-job-status` by REPO for every workspace repo that has no record here **and no row in this workspace's index**. The cloud follows its own repo -> job pointer and its body names the job, so the record is rebuilt from it and collected in the ordinary way. The index-row gate is what stops a finished resume recovering the same job forever: the pointer is written by a dispatch and cleared by nothing, and all three ways the record goes missing — a cleared `.carrick`, a second machine, a fresh clone — lose the index with it. A job that is over and answered nothing is not recovered for the same reason: it would be recorded, collected, forgotten and found again on every resume until the pointer expires (carrick#1320) |
| A job whose repo is not a workspace repo | Named and left alone. The build scans this workspace's repos, so collecting it would download the answers, scan nothing, and then forget the record — leaving the cloud row with no local handle at all. Resume it where that checkout is (carrick#1320) |
| A job whose driver stopped | `carrick resume` collects it like any other: the cloud serves the answers of a job in every state but queued, running and cancelled, and each pass wrote its part before it was killed, so a job that reads as `failed` usually holds most of its answers. The files it never answered are analysed by the build that collects it. Only a job the cloud answers for with no parts at all is forgotten, and its line says to hand the repo over again rather than to index it here — the machine that dispatched is the machine that could not (carrick#1319) |
| The stored index moved while the job ran | The resume finishes locally and does not upload: a newer index is not replaced by an older one. `carrick refresh` brings the newer one down. **The cloud decides this**, from the job's start time and the index rows' source, and says so on `analysis-job-answers` as `superseded` — a check-or-upload response says neither when a row landed nor what wrote it, and index rows carry no commit, so nothing on this side may derive it or name a commit. The run still opened a scan, so its last service ends one: `close-scan` with `reason: superseded` instead of the `scan_final` the skipped write would have carried, which hands the in-flight slot back and keeps the run out of the cloud's silent-scan sweep (carrick#1262, `src/cloud_storage/tee_storage.rs`). Best-effort, like the fail marker: a refusal is a debug line and the slot falls to its TTL, exactly as it did before the marker existed |

## Limits, stated

- **Intents are not in the bundle.** They are roughly one call per twenty
  functions once batched, and their prompts embed the answers from the level
  below, so they are generated by the machine that resumes.
- **The rows are held in memory** until the run ends — a few hundred megabytes
  on the largest repos, for the length of one dispatch (carrick#1244).
- **Nothing can guarantee a resumer exists.** If no machine ever collects a
  job, its answers sit in the cloud's content-addressed analysis cache until
  they expire. `carrick status` says so; that is a surface, not a mechanism.

## Changing a prompt body means the job runs again

The body is a name as well as a question, so moving one byte of it between a
dispatch and the resume renames every question the job answered: the stored
answers no longer match anything the resume rebuilds, so the resume joins
none of them and every file is analysed from the start. A job that took hours
the first time takes those hours again (carrick#1248). That is not avoidable
— only visible, and carrick#1332 is what makes it visible.

Three goldens hold the bytes:

- `tests/bundle_row_id_golden_test.rs` builds a real bundle over
  `tests/fixtures/llm-mocked-api`, offline, and asserts the row ids against
  `tests/golden/bundle-row-ids.json`.
- `file_analyzer_agent`'s own tests pin the digest of a body carrying every
  OPTIONAL section — the GraphQL hints, the imported wrappers, the postMessage
  block — which a plain HTTP fixture renders as zero bytes.
- `local_mode::jobs` reads `tests/golden/jobs-0.3.81.json` back: the record a
  customer's laptop is holding, which this release must still be able to read
  or the answers it names are unreachable.

A deliberate prompt or schema change updates the golden in the same PR, and
**the PR body says that every in-flight job will miss and be analysed again
from the start**. The failing test prints the list to check in.

## How it is proven

`scripts/dispatch-smoke.sh` runs the whole path live: it copies
`tests/fixtures/llm-mocked-api` into a tree of its own, appends a line unique
to the run to every source file so the cloud's content-keyed analysis cache
has no answer for any of them, commits, dispatches, waits for the driver, and
resumes. `.github/workflows/dispatch-smoke.yml` runs it nightly and on a PR
labelled `smoke:dispatch`; it needs a `cli`-scope credential and a repo of its
own to write to.

Two things make it the only test that can catch this class, and both are easy
to undo by accident:

- **The cache must be cold.** `--dispatch` submits nothing when every file is
  already answered (carrick#1251), so a smoke over a warm cache exercises the
  synchronous path and passes having dispatched nothing. The proof it
  dispatched is `.carrick/jobs.json`: it is written only from a submission the
  cloud accepted and named, so the smoke reads that, not the exit code.
- **It must authenticate as a laptop.** `CloudAuth::detect` prefers OIDC
  whenever the runner offers it, and an OIDC run opens no scan — so it holds no
  `scan_id`, which is exactly what `submit-analysis-job` refuses without. The
  workflow grants `contents: read` and nothing else for that reason.
