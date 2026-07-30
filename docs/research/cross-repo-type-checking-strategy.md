# Cross-repo type-checking: structural inlining vs real `.d.ts` emit

Carrick's compat check answers: does a consumer's expected type in repo A match the
producer's type in repo B. That is cross-*package* type compatibility. There are
two strategies for it, and we deliberately run the cheaper one. This note records
why, and the trigger that should make us revisit.

## Strategy A — per-op structural inlining (current)

Resolve each op's type with ts-morph and **inline** it into a synthetic alias:
`export type Endpoint_<hash>_Response = { id: number; amountCents: number; currency: string };`.
The cross-repo `.d.ts` is a pile of these per-op alias lines (no source
declarations); ts_check compares the inlined structural shapes.

- **Pros:** cheap (no dependency install, no per-service Program), and
  domain-appropriate (see "the domain nuance" below).
- **Cons:** structurally lossy (inlining `Money` to `{ amountCents; currency }`
  discards nominal identity, class privates, brands, recursion), and there are
  several resolution paths (explicit-symbol bundle, inference, placeholder) that
  each have to remember to produce the shape. Most of our type-pipeline bugs are
  "one of those paths emitted a bare/dangling name instead of the shape" (#246,
  #257, the producer-inference dangling names, the deterministic-anchor gap).

## Strategy B — real per-service `.d.ts` emit (the "proper" alternative)

Emit each service's real declaration file via the TypeScript compiler's
declaration emit (`Program.emit` / `emitToMemory({ emitOnlyDtsFiles: true })`),
exactly as `tsc -d` does when publishing a library, then type-check one service's
types against another's real declarations.

- **Why it is "proper":** (1) it is literally how TypeScript checks across
  packages (A imports B's emitted `.d.ts`); (2) one mechanism replaces the N
  fragile inlining paths, so named types simply carry their definitions and
  nothing dangles; (3) it preserves the compiler's exact (nominal) assignability
  rules.
- **Why it is shelved:** it was already built and retreated from. The dead
  `SurfaceEmitter` + `monorepo-builder.ts` in `src/sidecar/src/` are exactly this
  path (unreachable: the Rust `SidecarRequest` enum never constructs them). Its
  costs were real: per-check dependency install / a dependency snapshot, a
  synthetic workspace, `@carrick/{repo}/{spec}` specifier rewriting to avoid
  cross-repo name collisions, and run-to-run determinism. `dts-bundle-generator`
  was also tried and removed.

## The domain nuance (why "proper" is in quotes)

Carrick checks **JSON API contract drift**, and JSON erases nominal identity: a
branded `UserId` is just a `string` on the wire; class privates do not serialise.
So for the wire contract, structural comparison (Strategy A) is arguably the *more
correct* model, not merely the cheaper one. Strategy B is "proper" in compiler
theory, not obviously proper for this job.

## Decision (2026-06-26)

Stay on Strategy A and harden it incrementally (consumer-aware compat verdicts
[#260], extend structural expansion to the producer inference paths [#257],
deterministic anchor from the resolved ts-morph `Type` [#240]). Defer Strategy B
to a gated Phase 2, triggered ONLY if a post-batch residual is demonstrably
nominal/bundle-shaped, i.e. the inliner mangles a complex generic/recursive/branded
type in a way that produces a wrong verdict on a real contract. For plain JSON
shape drift, Strategy A is expected to be sufficient and arguably more honest.

## Revisit trigger — ts-go / TypeScript 7

The dominant objection to Strategy B was cost/boot-up (per-service Program
creation + emit + install). That calculus changes with the native Go compiler
(`microsoft/typescript-go`, "tsgo"/Corsa, shipping as TypeScript 7; 7.0 at RC as
of early 2026):

- ~7-10x faster full builds, and far faster Program load, which directly attacks
  the per-check cost that shelved Strategy B.
- It exposes a **programmatic API** (`@typescript/api`) — an embeddable compiler
  interface, not just the `tsgo` CLI. That is the surface a sidecar would need.
- Its **declaration emit was intentionally reworked** (closer to authored TS
  declarations / isolated-declarations style) — the exact feature Strategy B
  relies on.

So a future look is warranted, but it is a real migration, not a flag flip:
- The sidecar today uses **ts-morph**, a wrapper over the JS TypeScript compiler
  API. `@typescript/api` is a *different* API surface; moving the sidecar's
  backend to tsgo is a port, scoped and risk-assessed on its own.
- Verify at revisit time: `@typescript/api` stability/GA, that its declaration
  emit produces the self-contained `.d.ts` we want, and that cross-repo
  type-checking against multiple emitted services is supported and deterministic.

**Revisit when both hold:** (a) a Strategy-A residual proves to be a nominal /
bundle-shaped wrong verdict on a real contract, AND (b) tsgo's `@typescript/api`
is stable enough to build the per-service emit + cross-service check on.

## See also

- `docs/research/type-inference-pipeline.md` — the current end-to-end pipeline.
- `docs/research/compiler-sidecar-architecture/` — the original sidecar design
  (note: its code listings predate the shipped code; treat as historical).
