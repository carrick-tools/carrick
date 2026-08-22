# Running the scanner and its evals

Single entry point for every way this repo's scanner gets exercised: local
runs, the fixture-based accuracy evals, and OSS-repo runs. The companion index
for cloud-side eval systems (prompt harness, MCP tool-selection, OSS labels)
is `carrick-cloud/docs/internal/evals/README.md`.

All eval workflows are `workflow_dispatch`-only **monitors, never gates** —
each run makes real LLM calls, costs money, and is stochastic, so none of them
sit on the merge/PR path.

## Local scanner runs

```bash
cargo run -- /path/to/repo                              # real run (needs cloud auth)
CARRICK_MOCK_ALL=1 cargo run -- examples/express-single # offline demo, no network
```

See `AGENTS.md` for the full build/test command set.

## Tier-A framework evals

Scores endpoint/call P/R/F1 for the Tier-A framework fixtures (koa-api,
fastify-api, hapi-api, nestjs-api…) against each fixture's `expected.json`.

- Scorer: `tests/eval_tier_a.rs` (`--ignored` integration test).
- CI: `gh workflow run eval-tier-a.yml` (inputs: `runs`, `capture`).
- Pinned baseline: `tests/eval/baseline.jsonl`.
- Run records land in `target/eval-runs/run-*.jsonl` and are pushed to the
  Axiom eval history via carrick-cloud's `POST /eval/ingest` (OIDC-gated;
  non-blocking).

Local invocation (fixture deps must be `npm install`ed first):

```bash
CARRICK_API_ENDPOINT=https://api.carrick.tools \
CARRICK_EVAL_RUNS=5 \
cargo test --release --test eval_tier_a -- --test-threads=1 --nocapture
```

## Cross-repo eval (`eval_xrepo`)

Scores the live scanner over an authored multi-repo corpus and reports the
full per-metric correctness vector (endpoint set, call set, decoy leaks, owner
accuracy, type anchors, type resolution, cross-repo matches, compat verdicts —
see the scorer header in `tests/eval_xrepo.rs` and the contract in
`carrick-cloud/docs/internal/decisions/cross-repo-eval-scorer-contract.md`).

- Corpora: `tests/fixtures/xrepo-corpus-1` (authored 3-repo, default),
  `xrepo-corpus-2` (event-driven pub/sub, 5 repos), `xrepo-corpus-3`
  (messy-realism, 7 repos). Each corpus README documents its answer-key
  conventions; `expected-output.json` + per-repo `expected.json` are the
  spec-of-record labels.
- CI: `gh workflow run eval-xrepo.yml` (inputs: `runs`, `corpus`).
- Offline harness: `tests/xrepo_harness_test.rs` (corpus via
  `CARRICK_XREPO_CORPUS`, no LLM calls).

Local live invocation (corpus repo deps installed, sidecar built — the
workflow steps in `.github/workflows/eval-xrepo.yml` are the reference
sequence):

```bash
CARRICK_API_ENDPOINT=https://api.carrick.tools \
CARRICK_EVAL_LIVE=1 CARRICK_EVAL_RUNS=5 \
CARRICK_EVAL_CORPUS=xrepo-corpus-1 CARRICK_SKIP_INTENTS=1 \
cargo test --release --test eval_xrepo -- \
  --ignored xrepo_live_scorer --test-threads=1 --nocapture
```

## OSS-repo runs (`eval-oss.yml`)

Anti-overfit runs against third-party OSS repos cloned at pinned SHAs. The
workflow materializes the repos into `tests/fixtures/oss-corpus/` from a
dispatch input, writes ground-truth labels from `labels_json`, installs deps
best-effort (scripts disabled), and runs the same `eval_xrepo` live scorer
with `CARRICK_EVAL_CORPUS=oss-corpus`. Nothing naming the scanned projects is
committed in this repo.

```bash
gh workflow run eval-oss.yml --ref main \
  -f repos='<name>=<https url>@<sha>' \
  -f labels_json="$(cat <labels file>)" \
  -f runs=1
```

Targets, pinned SHAs, label slices, and the run-by-run results log are
private: `carrick-cloud/docs/internal/evals/oss-eval/RUNBOOK.md`. The workflow
builds the scanner from the dispatched ref, so fix branches can be verified
pre-merge. **Precision is meaningless on partial-label OSS slices** — read
recall per edge from the `[diag]` lines, never the run mean.

Label conventions the scorer normalizes for you:

- **Edge repo identity is the corpus repo dir name.** The projection's
  `producer_repo`/`consumer_repo` carry the `service_name ?? repo_name` id, so
  the scorer folds every `carrick.json` service name to its owning corpus repo
  dir on both sides of the match/compat/orphan join. Labels may use either
  form. If labels and predictions share zero joined edges while both are
  non-empty, the scorer prints a loud `[warn]` zero-join diagnosis instead of
  a silent 0.00.
- **Compat is UNSCORED when no labelled edge carries a `type_compatible`
  verdict** (the usual state for OSS slices) — never a vacuous 1.00.

## Eval env vars

| Var | Meaning |
|---|---|
| `CARRICK_EVAL_LIVE=1` | Enables the live (real-LLM) xrepo scorer path |
| `CARRICK_EVAL_RUNS=N` | Repeated scans per invocation (variance / pass^k) |
| `CARRICK_EVAL_CORPUS=<dir>` | Corpus fixture dir name under `tests/fixtures/` |
| `CARRICK_EVAL_CAPTURE=1` | Tier-A: dump raw file-analyzer input/output per run |
| `CARRICK_EVAL_DUMP_DIR=<dir>` | Persist raw analyzer I/O for prompt diagnosis |
| `CARRICK_SKIP_INTENTS=1` | Skip intent generation (dominant cost term; no eval dimension consumes intents) |
| `CARRICK_INTENT_CONCURRENCY=N` | Concurrent `/generate-intent` calls per dependency level (default 8, capped by `CARRICK_CONCURRENCY_LIMIT`) |
| `CARRICK_XREPO_CORPUS=<path>` | Offline harness corpus override |
| `CARRICK_MOCK_ALL=1` | Fully offline scanner run (mocked LLM responses) |
| `CARRICK_LOCAL_STORAGE_DIR` | Local upload cache dir — eval runs never write the real cloud index |

## Where results go

- CI logs print the score report; workflow artifacts carry
  `target/eval-runs/*.jsonl` for post-run diagnosis.
- Tier-A / xrepo corpus history is durable in the Axiom `carrick-evals`
  dataset (carrick-cloud `eval-ingest`).
- OSS runs are deliberately NOT pushed to Axiom; findings are logged in the
  per-target program files under `carrick-cloud/docs/internal/evals/oss-eval/`.
- Never commit run outputs to either repo.
