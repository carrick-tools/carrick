# docs/ — map and placement rules

Read this when you need to find existing documentation or add new
documentation; nothing else requires it.

Documentation for the public Carrick scanner. The private companion repo
(`carrick-cloud`) uses the same layout under `docs/internal/`; user-facing
product docs live there in `docs-site/` (docs.carrick.tools), not here.

Repo-wide truth lives at the root, not in `docs/`:

- `CLAUDE.md` — hard rules (public/private boundary, prompt-leak guard, no
  backwards compat).
- `AGENTS.md` — canonical repo guidelines: project structure, build/test
  commands, coding style, commit conventions.
- `README.md` — the public product README.

## Layout

| Location | What belongs there | Lifecycle |
|---|---|---|
| [`evals.md`](./evals.md) | How to run every eval and scanner run this repo hosts (local runs, Tier-A, cross-repo, OSS dispatch) | Kept current |
| [`local-mode-output.md`](./local-mode-output.md) | The `carrick.check/0` output contract for local mode (`touch`/`check`), with its JSON schema in [`schemas/`](./schemas/) | Kept current: additive changes only |
| [`reference/`](./reference/) | Durable explanations of how a subsystem works | Kept current: update in place, delete when obsolete |
| [`archive/`](./archive/) | Shipped plans, handoffs, historical design records | Frozen: never updated, kept for context only |

Component docs stay next to their component: `src/sidecar/README.md` (the
TypeSidecar), `tests/fixtures/xrepo-corpus-*/README.md` (eval corpora and
their answer-key conventions).

## Placement rules (for agents adding a doc)

1. **Explaining how something works?** → `reference/`, or the component's own
   README if it's component-scoped. Extend an existing doc before creating a
   new file. A reference doc must be pointed to by a comment at the code it
   governs — code work reaches docs through those pointers, not by browsing
   this tree, so an unreferenced reference doc is invisible: link it or
   delete it.
2. **Explaining how to run something?** → `evals.md` for eval/run procedure;
   `AGENTS.md` for everyday build/test commands.
3. **Planning or handing off work?** → a GitHub issue, not a doc. If a plan
   genuinely needs a doc (large design brief), it carries a `Status:` header
   and moves to `archive/` the moment it ships.
4. **Run outputs?** → never committed. Eval results live in CI logs, workflow
   artifacts, and the Axiom eval history. Committed JSON under
   `tests/fixtures/` is answer keys (inputs), not output.

Anything in `archive/` may describe code that no longer exists — trust the
code and the docs outside `archive/` first.

## Cross-repo pointers

Eval systems whose commands live in `carrick-cloud` (file-analyzer prompt
harness, MCP tool-selection eval, OSS-eval labels + runbook) are indexed at
`carrick-cloud/docs/internal/evals/README.md`. The cross-repo scorer's decided
contracts live at `carrick-cloud/docs/internal/decisions/`.
