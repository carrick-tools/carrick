# Repository Guidelines

Canonical instructions for every coding-agent harness: Claude Code imports
this file through `CLAUDE.md`, Codex reads it directly. Edit this file, not
`CLAUDE.md`. This is the **public** Rust scanner for Carrick; the private
companion `carrick-cloud` holds Lambdas, Terraform and the dashboard.

## Carrick (this repo)

- Project slug if opened alone: `project: "scanner-evals"` (workspace `daveymoores`). The index reads each repo's main, so build against a sibling's contract from the index; unmerged local state need not be visible.
- The index cannot see the Rust scanner as a service (no HTTP surface), so a nil result for scanner-internal behaviour is expected; say so rather than skipping the call.
- Write correct, idiomatic, explicitly typed code; never contort code so the scanner can read it. If Carrick fails to extract something written normally, that is a Carrick bug to report, not a constraint to code around.

## Core Goals (Read First)
- Build a live, type-aware, intent-aware index of TypeScript services across a GitHub org. The index is the product surface; AI coding agents query it over MCP.
- The Rust scanner in this repo is the index-population component. Per scanned function it extracts structural facts, request/response types, and intent.
- The GitHub Action wraps the scanner and runs per repo. PR comments showing cross-repo drift are the demoable proof point of the type analysis, not the headline product.
- Build framework- and library-agnostic REST extraction for TypeScript services. The structural layer of the index depends on this; correctness in cross-repo matching takes priority over framework-specific heuristics.
- Remove legacy code paths; backwards compatibility is not required and dead code should not be kept.

## Project Structure & Module Organization
- `src/` is the main Rust crate (engine, parser, analysis, cloud storage).
- `crates/carrick-match/` is the path-matching crate (`paths_match`, `match_agreement`, `path_literal_specificity`, `is_catch_all_path`, `is_unknown_call_path`, `is_param_segment`, `match_verdict`, `reportable_agreement`) — the single source of matching semantics for both sides of Carrick; never fork matching logic elsewhere. A surface deciding whether to REPORT a pair as a match calls `reportable_agreement` (or `match_verdict` for the reason); `paths_match` answers routing truth only and `match_agreement` returns `Some(0)` for pairs that route without corroborating anything. The scanner depends on it natively. Every release also compiles it to wasm (feature `wasm`) and attaches `carrick_match.js` / `carrick_match.d.ts` / `carrick_match_bg.wasm` / `carrick_match.sha256` as release assets via `.github/workflows/wasm-artifact.yml` (also runnable by `workflow_dispatch` to backfill a tag). The companion cloud repo pins those assets by hash and runs the same matcher at query time, so semantics changes here propagate on the next release. The crate's `wasm-bindgen` dependency is pinned exactly; the same version is pinned as the `wasm-bindgen-cli` install in BOTH `.github/workflows/ci.yml` and `.github/workflows/wasm-artifact.yml` — bump all three together.
- `tests/` holds Rust integration/unit tests; fixtures live in `tests/fixtures/`.
- `src/sidecar/` is the TypeScript type-extraction sidecar with tests in `src/sidecar/test/`. Every release also bundles it into one file, `carrick-lister.mjs`, with `carrick-lister.manifest.json` (`src/sidecar/lister/build.mjs`, run by `.github/workflows/wasm-artifact.yml`): the surface lister the companion cloud repo's library store pins by hash and runs on Node 22.
- Cross-repo type compatibility runs through the sidecar's capture_v2/check_v2 actions ("tsc as serializer/judge"): each service's scan emits a compiler-emitted declaration stub, and cross-repo analysis typechecks generated probes in a synthetic workspace (`src/engine/type_compat_v2.rs` drives it). The legacy `ts_check/` comparison layer was deleted in the v2 flip; `docs/reference/type-checking-flow.md` describes it for history only.
- `scripts/` contains developer tooling (pre-commit hook installer).
- `docs/` — consult `docs/README.md` (the map) before reading or placing any doc; `docs/evals.md` covers running every eval.
- `action.yml` defines the GitHub Action entrypoint.
- `examples/` contains reference Express services used as e2e fixtures by CI and as user-facing demos.

Lambdas, MCP server, AWS infrastructure, and the web dashboard live in the companion `carrick-cloud` repo, not here.

## Build, Test, and Development Commands
```bash
# Build and run
cargo build
cargo build --release
cargo run -- /path/to/repo
CARRICK_MOCK_ALL=1 cargo run -- examples/express-single

# Tests and checks
cargo test
cargo test --test integration_test
cargo fmt
cargo clippy

# Evals (Tier-A, cross-repo, OSS runs): see docs/evals.md

# Type sidecar (Node)
cd src/sidecar
npm install
npm ci --prefix lister   # bundler for the surface lister artifact test
npm run build
npm test
```
Install hooks once per clone, and again from a checkout of main after `scripts/hooks/` changes: `./scripts/install-hooks.sh`.

## Coding Style & Naming Conventions
- Rust is formatted with `rustfmt`; keep code `cargo fmt` clean and `clippy`-warning free.
- Use `snake_case` for modules/functions/files, `CamelCase` for types, and `SCREAMING_SNAKE_CASE` for constants.
- TypeScript follows standard `camelCase`/`PascalCase` conventions; mirror nearby files.

## Testing Guidelines
- Rust tests live in `tests/*.rs` (files typically end in `_test.rs`); fixtures are under `tests/fixtures/`.
- Use `CARRICK_MOCK_ALL=1` for tests that should avoid real cloud storage.
- Sidecar tests are authored in `src/sidecar/test/` and run via `npm test` after `npm run build`.
- Follow TDD: add/adjust tests first, then implement changes.
- Tests, formatting, and build steps must pass after each phase of work.

## Commit & Pull Request Guidelines
- Follow Conventional Commits with optional scopes: `feat(sidecar): ...`, `fix: ...`, `docs: ...`, `refactor(phase4.1): ...`, `chore: ...`, `test: ...`.
- PRs should include a short summary, rationale, test commands run, and links to relevant issues. Include sample Action output when it changes.

## Ownership Boundary (who does what)
Agents own the delivery loop end to end: branch, PR, review of the full diff, reading the real CI check conclusions, squash-merge, and cutting the release by merging the release-please PR. The release PR opens as a draft (`draft-pull-request` in `release-please-config.json`), so cutting a release is `gh pr ready <n>` and then the merge — the draft is the pause in which the deploy-order check happens, since a blob shape the deployed reader drops must not ship before the cloud side of it is live (carrick#840). The same pause holds the release guard (carrick#1734): the release agent replays the candidate over the golden benchmark repos with their stored model answers, offline and at $0, with the private guard kept in the companion cloud repo, and names every change the candidate makes there in advance. A release PR with any guard FAIL is neither marked ready nor merged. The guard runs on the release agent's machine, never in CI. Nothing in that loop waits on the owner. Exactly two actions are the owner's and must be requested, never performed by an agent:
- **Cloud deploys**: `terraform apply` and the lambda/app zips it ships, in `carrick-cloud`. Merged is not deployed; say so explicitly when a change needs a deploy before it can be measured.
- **Paid eval spend**: any run that makes real LLM calls (Tier-A, cross-repo, OSS evals, corpus sweeps, fleet re-scans). State the estimated cost and get an explicit yes on that number before launching.

## Estimation Conventions
- Effort estimates in issues, plans, and design docs use S/M/L defined in agent execution time. Claude agents execute this repo's work, so estimates are not in human developer time.
  - S: one agent, merged PR in under about an hour of wall clock. Implementation is minutes; CI (~5-10 min per push) dominates.
  - M: one agent working session, roughly 1-3 hours to a merged PR, usually one or two review/rebase cycles.
  - L: a sustained orchestrated session (parallel agents where the work decomposes), roughly a day.
- Converting older docs or human-calibrated estimates: a human-week of focused work is roughly one L session; a human-month is roughly 3-5 sustained sessions spread over about a week of elapsed time.
- Elapsed calendar time is set by the serialization points, not implementation speed: CI runs, review latency, dependency chains between work packages, same-release bundles, and the two owner-gated actions (cloud deploys via terraform apply, and paid eval spend including fleet re-scans; see Ownership Boundary). Merges and releases are not serialization points: agents perform them. Estimates should name their serialization points.
- Calibration (2026-07-18/19): nine scoped S/M tickets merged in one overnight session; a six-item tier including an L-sized prototype spike completed in about three hours of wall clock.

## Configuration & Infrastructure Notes
- `carrick.json` is resolved by `Config::load_services` into one `Config` per service. A flat config (or none) is a single service rooted at the repo root; a `services` array fans out per directory (`directory`, `include`, `tsconfig`, `graphqlSchemas` + the call-classification fields). The engine runs the analysis pipeline, per-service type extraction, and upload once per service. Multi-service index upload is gated on `CloudStorage::supports_multi_service`, driven by the cloud's `multiService` capability flag. See the README "Monorepos" section for the user-facing shape.
- Runtime env vars: `ACTIONS_ID_TOKEN_REQUEST_URL` / `ACTIONS_ID_TOKEN_REQUEST_TOKEN` (auto-set by GitHub Actions when the job grants `id-token: write`; the scanner mints an OIDC token from these and sends it as the `X-Carrick-OIDC` header — the cloud derives repo identity from the signed claims, so no API key is needed), `CARRICK_MOCK_ALL` (test-only, returns canned responses without hitting the cloud), `CARRICK_API_ENDPOINT` (override the default `https://api.carrick.tools` endpoint at build time; optional).
- Terraform, Lambdas, and dashboard code live in `carrick-cloud`. No infrastructure or server-side code belongs in this repo.

## Hard rules

- **Never run `terraform` shell commands.** Terraform and the rest of the AWS infrastructure live in `carrick-cloud`, not here. If a task needs infra changes, switch to that repo.
- **No LLM system instructions in this repo.** Per the public/private split, system-prompt strings live in `carrick-cloud/lambdas/*/system_prompt.txt`. User-message templates that interpolate scan-time data may live in Rust because they need access to the data structures the scanner produces (e.g. `src/agents/file_analyzer_agent.rs`). CI workflow `prompt-leak-guard.yml` enforces this as a ratchet against `.github/prompt-leak-baseline.txt`: counts may shrink but never grow. It scans every `*.rs` file under `src/`, `build.rs`, and `tests/` (excluding `tests/fixtures/`) for the patterns `You are `, `You describe `, `You analyze `, `Extract ONLY`, `responseSchema`, `system_instruction`, `prompt:[[:space:]]*"`, `Identify all frameworks`, and `"frameworks":`.
- **The READMEs are GTM copy.** `README.md` and `npm/carrick/README.md` are rendered by GitHub, the Marketplace, npm and the MCP directories, and their wording was agreed with the owner line by line. Change them only when a surface, a task skill or an MCP tool is added, removed or renamed, and then only the matching section or table row. Reference detail belongs on docs.carrick.tools. Any other change, including style or accuracy fixes, goes to the owner as a proposal first.
- **No backwards compatibility / no users.** When refactoring, ship the new shape and delete the old shape in the same commit. No feature flags, no deprecation cycles, no parallel old/new code paths.

## Boundary

- Public (this repo): Rust scanner, AST/parser, agent orchestrators (thin), `src/sidecar/`, GitHub Action.
- Private (`carrick-cloud`): all Lambdas, MCP server + tools, Terraform, prompts, wrapper-rule generation, future web dashboard.

MCP is exposed exclusively as an HTTP endpoint at `https://api.carrick.tools/mcp`. Users add Carrick to their AI agent via `claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp`. There is no local-stdio install — the MCP tool implementations live in `carrick-cloud/lambdas/mcp-server/`.

If you need to touch a Lambda, Terraform, or a prompt, the change goes in `carrick-cloud`.

## Where things are

Reading or writing documentation, or running evals? Start at `docs/README.md` — the map. It says where everything lives and where new docs go; don't place a doc without it. Otherwise ignore `docs/`.

The Carrick → carrick-cloud split landed in 2026-05. Follow-up work (OAuth dashboard, ELv2 relicense + flip public, etc.) is tracked as GitHub issues.

<!-- carrick:check start -->
## Carrick: use it while you work, not only at the end

This workspace (one repository, 1 service) is indexed by Carrick. The
`mcp__carrick__*` tools answer questions about code outside the file you are
editing more completely than grep: across every service and every protocol
(HTTP routes, socket events, pub/sub topics), including callers reached through
SDKs, wrappers, client instances and untyped helpers that share no searchable
name with what you changed. Pass `project: "scanner-evals"` on every Carrick
call.

1. Before you write a new function, helper or utility, call `search_by_intent`
   (or `find_similar`) with a plain-English description of what it does. If
   something in any package already does the job, or most of it, import it and
   call it instead of writing your own. When the task names a new function or
   file, still create it, but make its body call the existing function and add
   only what the existing one lacks. Never leave a second copy of logic that
   already exists.
2. Before you change a route, a request or response shape, an event payload or
   an exported function, call `get_operation` (method and path, or the event or
   topic name) or `get_callers`, and update or explicitly clear every consumer
   it lists, in every repository.
3. Before you write code that calls another service's endpoint or handles its
   event, call `get_endpoint_types` (or `get_contract_pair`) and use the real
   request and response types it returns. Never take a shape from the ticket, a
   similar endpoint or another API version, or a local copy of a type.
4. Use grep and Read to confirm and to read code once Carrick has told you
   where to look.
5. Run `carrick check --changed` before you say it is done only when your change
   touches a route, a call to another service, an event or a type another
   service reads. Each file you edit is already checked for you, so a change no
   other service sees needs nothing more. In your final summary, say what
   Carrick found and what you changed because of it.
<!-- carrick:check end -->
