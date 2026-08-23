# Repository Guidelines

## Carrick

**If the answer is defined by another service — its endpoints, its real
request/response types, who consumes it, or any local type, constant, or
SDK helper that mirrors it — ask Carrick before you rely on anything
local.** Local copies of another service's contract drift and lie; the
producing service defines the contract, and if two definitions disagree,
the producer's definition wins. Load the Carrick tools now so they are ready at that
moment.

**Inside this repo, grep and read are faster — use them.** One exception:
before writing any new helper, parser, validator, or domain function, run
`search_by_intent` with a plain-English description of the behaviour.
Grep can prove a name is absent; it can never prove the behaviour is.

Then, before you write new code, state one line naming what you asked and
what came back:

`Carrick checked: <query> -> <result count>`

The loop for cross-service work: `get_service_graph` (who calls whom) →
`search_by_intent` (prior art) → `get_endpoint_types` (real contracts) →
build → `check_compatibility` (each consumer, before changing a response
shape, removing an endpoint, or renaming a path). Before adding or bumping
an npm package, `get_service_dependencies` surfaces cross-service version
conflicts at design time. For audit-shaped work, such as inventorying what
a service calls outside itself, `list_external_calls` enumerates the
recorded outbound-call candidates (SDK calls, external domains, env-var URL
bases) with their file and line.

Carrick indexes every service in the project **scanner-evals**
(workspace **daveymoores**): every function with an intent
description, its dependencies, and API endpoints with their real
request/response types, on each repo's main branch, exported or not. Pass
`project: "scanner-evals"` (or `repo: "<owner/repo>"` from the git
remote) on every call. Carrick is read-only; data reflects the most recent
scan of each repo. The index at main is what production integrates
against, so build against a sibling's contract from the index and merge —
your unmerged local state does not need to be visible to Carrick.

Write correct, idiomatic, explicitly typed code; never contort code so the
scanner can read it. If Carrick fails to extract something written
normally, that is a Carrick bug to report, not a constraint to code
around.

### Connect the agent

```
claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp
```

One install serves every project in the workspace.


## Core Goals (Read First)
- Build a live, type-aware, intent-aware index of TypeScript services across a GitHub org. The index is the product surface; AI coding agents query it over MCP.
- The Rust scanner in this repo is the index-population component. Per scanned function it extracts structural facts, request/response types, and intent.
- The GitHub Action wraps the scanner and runs per repo. PR comments showing cross-repo drift are the demoable proof point of the type analysis, not the headline product.
- Build framework- and library-agnostic REST extraction for TypeScript services. The structural layer of the index depends on this; correctness in cross-repo matching takes priority over framework-specific heuristics.
- Remove legacy code paths; backwards compatibility is not required and dead code should not be kept.

## Project Structure & Module Organization
- `src/` is the main Rust crate (engine, parser, analysis, cloud storage).
- `crates/carrick-match/` is the path-matching crate (`paths_match`, `match_agreement`, `path_literal_specificity`, `is_catch_all_path`, `is_param_segment`) — the single source of matching semantics for both sides of Carrick; never fork matching logic elsewhere. The scanner depends on it natively. Every release also compiles it to wasm (feature `wasm`) and attaches `carrick_match.js` / `carrick_match.d.ts` / `carrick_match_bg.wasm` / `carrick_match.sha256` as release assets via `.github/workflows/wasm-artifact.yml` (also runnable by `workflow_dispatch` to backfill a tag). The companion cloud repo pins those assets by hash and runs the same matcher at query time, so semantics changes here propagate on the next release. The crate's `wasm-bindgen` dependency is pinned exactly; the same version is pinned as the `wasm-bindgen-cli` install in BOTH `.github/workflows/ci.yml` and `.github/workflows/wasm-artifact.yml` — bump all three together.
- `tests/` holds Rust integration/unit tests; fixtures live in `tests/fixtures/`.
- `src/sidecar/` is the TypeScript type-extraction sidecar with tests in `src/sidecar/test/`.
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
npm run build
npm test
```
Install hooks once per clone: `./scripts/install-hooks.sh`.

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

## Estimation Conventions
- Effort estimates in issues, plans, and design docs use S/M/L defined in agent execution time. Claude agents execute this repo's work, so estimates are not in human developer time.
  - S: one agent, merged PR in under about an hour of wall clock. Implementation is minutes; CI (~5-10 min per push) and Copilot review dominate.
  - M: one agent working session, roughly 1-3 hours to a merged PR, usually one or two review/rebase cycles.
  - L: a sustained orchestrated session (parallel agents where the work decomposes), roughly a day.
- Converting older docs or human-calibrated estimates: a human-week of focused work is roughly one L session; a human-month is roughly 3-5 sustained sessions spread over about a week of elapsed time.
- Elapsed calendar time is set by the serialization points, not implementation speed: CI runs, review latency, dependency chains between work packages, same-release bundles, batched paid eval gates, and owner-gated actions (terraform applies in carrick-cloud, deploys, releases, fleet re-scans). Estimates should name their serialization points.
- Calibration (2026-07-18/19): nine scoped S/M tickets merged in one overnight session; a six-item tier including an L-sized prototype spike completed in about three hours of wall clock.

## Configuration & Infrastructure Notes
- `carrick.json` is resolved by `Config::load_services` into one `Config` per service. A flat config (or none) is a single service rooted at the repo root; a `services` array fans out per directory (`directory`, `include`, `tsconfig` + the call-classification fields). The engine runs the analysis pipeline, per-service type extraction, and upload once per service. Multi-service index upload is gated on `CloudStorage::supports_multi_service`, driven by the cloud's `multiService` capability flag. See the README "Monorepos" section for the user-facing shape.
- Runtime env vars: `ACTIONS_ID_TOKEN_REQUEST_URL` / `ACTIONS_ID_TOKEN_REQUEST_TOKEN` (auto-set by GitHub Actions when the job grants `id-token: write`; the scanner mints an OIDC token from these and sends it as the `X-Carrick-OIDC` header — the cloud derives repo identity from the signed claims, so no API key is needed), `CARRICK_MOCK_ALL` (test-only, returns canned responses without hitting the cloud), `CARRICK_API_ENDPOINT` (override the default `https://api.carrick.tools` endpoint at build time; optional).
- Terraform, Lambdas, and dashboard code live in `carrick-cloud`. No infrastructure or server-side code belongs in this repo.
