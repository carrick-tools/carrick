# CLAUDE.md — `carrick`

This is the **public** Rust scanner for Carrick. Companion to the private `carrick-cloud` repo (Lambdas + Terraform + dashboard).

## Carrick

This repo is part of the Carrick project **scanner-evals** (workspace
**daveymoores**), alongside `carrick-cloud` and `carrick-site`. Carrick
indexes every function in each repo with an intent description, its
dependencies, and API endpoints with their real request/response types, on
each repo's main branch, exported or not. Pass `project: "scanner-evals"`
(or `repo: "<owner/repo>"` from the git remote) on every call.

**If the answer is defined by another repo (what the cloud reads from the
index blob, the extraction response schema, the wire envelope, or any local
type or constant that mirrors them), ask Carrick before you rely on
anything local.** Local copies of another service's contract drift; the
producing side defines the contract.

**Inside this repo, grep and read are faster; use them.** One exception:
before writing any new helper, parser, validator, or domain function, run
`search_by_intent` with a plain-English description of the behaviour. Grep
can prove a name is absent; it can never prove the behaviour is. Then,
before you write new code, state one line naming what you asked and what
came back:

`Carrick checked: <query> -> <result count>`

The index cannot see the Rust scanner as a service (no HTTP surface), so a
nil result for scanner-internal behaviour is expected; say so rather than
skipping the call. Write correct, idiomatic, explicitly typed code; never
contort code so the scanner can read it. If Carrick fails to extract
something written normally, that is a Carrick bug to report, not a
constraint to code around.

### Connect the agent

```
claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp
```

One install serves every project in the workspace.

## Hard rules

- **Never run `terraform` shell commands.** Terraform and the rest of the AWS infrastructure live in `carrick-cloud`, not here. If a task needs infra changes, switch to that repo.
- **No LLM system instructions in this repo.** Per the public/private split, system-prompt strings live in `carrick-cloud/lambdas/*/system_prompt.txt`. User-message templates that interpolate scan-time data may live in Rust because they need access to the data structures the scanner produces (e.g. `src/agents/file_analyzer_agent.rs`). CI workflow `prompt-leak-guard.yml` enforces this as a ratchet against `.github/prompt-leak-baseline.txt`: counts may shrink but never grow. It scans every `*.rs` file under `src/`, `build.rs`, and `tests/` (excluding `tests/fixtures/`) for the patterns `You are `, `You describe `, `You analyze `, `Extract ONLY`, `responseSchema`, `system_instruction`, `prompt:[[:space:]]*"`, `Identify all frameworks`, and `"frameworks":`.
- **No backwards compatibility / no users.** When refactoring, ship the new shape and delete the old shape in the same commit. No feature flags, no deprecation cycles, no parallel old/new code paths.

## Boundary

- Public (this repo): Rust scanner, AST/parser, agent orchestrators (thin), `src/sidecar/`, GitHub Action.
- Private (`carrick-cloud`): all Lambdas, MCP server + tools, Terraform, prompts, wrapper-rule generation, future web dashboard.

MCP is exposed exclusively as an HTTP endpoint at `https://api.carrick.tools/mcp`. Users add Carrick to their AI agent via `claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp`. There is no local-stdio install — the MCP tool implementations live in `carrick-cloud/lambdas/mcp-server/`.

If you need to touch a Lambda, Terraform, or a prompt, the change goes in `carrick-cloud`.

## Where things are

`AGENTS.md` is the canonical repo-guidelines doc — read it for project structure, build commands, testing conventions, and commit style.

Reading or writing documentation, or running evals? Start at `docs/README.md` — the map. It says where everything lives and where new docs go; don't place a doc without it. Otherwise ignore `docs/`.

The Carrick → carrick-cloud split landed in 2026-05. Follow-up work (OAuth dashboard, ELv2 relicense + flip public, etc.) is tracked as GitHub issues.
