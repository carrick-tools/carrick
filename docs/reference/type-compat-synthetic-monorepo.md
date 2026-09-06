# Type Compatibility v2 — Compiler-Native Artifacts in a Synthetic Monorepo

**Status:** Proposed (2026-07), hardened by three adversarial review passes
(compiler mechanics — claims tested empirically against tsc; codebase
fact-check — every file/line reference verified; migration/ops). See
"Review record" at the end.
**Supersedes (on completion):** the flat-bundle + `ts_check` comparison path
**Builds on:** `docs/archive/compiler-sidecar-architecture/` (Phase 3, "Synthetic
Monorepo / Stub Snapshot"), `docs/reference/type-checking-flow.md`

## Executive summary

The comparison engine is not the problem. `Type.isAssignableTo()` on real
compiler `Type` objects (`ts_check/lib/type-checker.ts:456`) is the right
verdict function, and the manifest matcher (method + normalized path +
type_kind, plus exact operation keys for socket/graphql/pubsub) is sound.

What makes type compat persistently hard is upstream of the verdict: **the
cross-repo artifact is a hand-serialized string.** Types are captured by
printing them to text with a custom printer (`expandTypeStructural`), shipped
as one flattened `.d.ts` string per service (`CloudRepoData.bundled_types`),
and re-parsed at check time in a synthetic flat project where the original
imports do not exist. Every hop is lossy, and nearly every hard-won fix in the
current system is compensation for that loss:

| Compensation | Root cause it patches |
|---|---|
| `expandTypeStructural` (custom recursive printer, depth cap 12) | named refs dangle in a flattened bundle → `any` |
| `append_missing_aliases` + `// carrick:missing-alias` marker (byte-identical across Rust and TS, `src/engine/mod.rs` ↔ `ts_check/lib/type-checker.ts:34`) | unresolved anchors must be distinguishable from real `unknown` (#244) |
| double `any`/`unknown` guards in `compareTypes` (raw types, then comparands) | dangling names resolve to top types → vacuously "compatible" |
| `cleanupPaths` regex in `ts_check/run-type-checking.ts` | absolute `import("...")` specifiers leak into error text |
| merged `package.json` + `npm install --legacy-peer-deps` | library types kept by-name must resolve *somewhere* at check time |
| verdict join by re-parsing endpoint label strings (`src/analyzer/mod.rs`, `parse_compat_endpoint`) | no stable pair identity crosses the Rust/TS boundary |
| formatter regex-parsing mismatch strings back apart (`src/analyzer/mod.rs:2220` ↔ `src/formatter/mod.rs:832`) | verdict detail travels as prose, not structure |

We are incrementally reimplementing TypeScript's declaration emitter — one of
the hairiest parts of the compiler — one regression at a time (#149, #226,
#233, #240, #244, #253, #260 are all inline-documented instances).

**Proposal:** finish the synthetic-monorepo architecture that is already
partially built but unreachable from Rust (`MonorepoBuilder`, `SurfaceEmitter`,
tsconfig/dependency snapshots — the sidecar handles `emit_surface`,
`build_workspace`, `check_compatibility`, but `SidecarRequest` in
`src/services/type_sidecar.rs:161-191` never sends them), with two corrections
to its current design:

1. **The artifact becomes a compiler-emitted declaration tree** (a types-only
   package per service), not a flattened structurally-expanded string. Let
   `tsc` be the serializer.
2. **The checker becomes per-pair probe files** checked by `tsc --noEmit` in
   the workspace, with diagnostics attributed by filename + diagnostic code.
   Let `tsc` be the judge, and let its elaborated errors be the user-facing
   mismatch report.

Everything the pipeline currently does to type *text* by hand should either
become a real file the compiler emitted or a real probe the compiler checked.

Since Carrick has no users and no backwards-compatibility obligations, the old
path is deleted in the same release that ships the new one. Previously
uploaded artifacts are simply ignored (join-time artifact-version check) and
the fleet re-scans once.

## Framing

Cross-repo type checking at different scan times is the problem npm publishing
already solves: a types-only package with pinned dependencies, installed and
consumed later by a different project. Each repo's scan should behave like
publishing `@carrick/<service>` — a package whose public API is the service's
endpoint contract. The cross-repo check is then "install all captured packages
into one workspace and typecheck generated probes."

This is the same insight as the existing Phase-3 stub-snapshot design; the
corrections are *what goes inside the stub package* and *how verdicts are
attributed*.

## Current state (what actually runs today)

Per service, at scan time (all in the source repo, with `node_modules`
available whenever the tree carries them — in CI the Action installs them
before the scan when a lockfile is present, `scripts/install-scanned-deps.sh`;
when it cannot, capture runs bare and step 8 below says what that costs):

1. LLM file analysis emits type anchors per endpoint/call
   (`primary_type_symbol`, `type_import_source`, expression text, SWC spans).
2. `FileOrchestrator::collect_type_requests` turns anchors into sidecar
   requests: explicit `SymbolRequest`s, inline aliases, or `infer` requests.
3. The sidecar resolves them with ts-morph and serializes each result to
   structural text (`expandTypeStructural`: objects inlined recursively,
   library types kept by name). `TypeBundler` — `@deprecated` but live
   (`handleBundle` uses it) — renames declarations via first-occurrence
   `String.replace`.
4. Rust concatenates the results into one `.d.ts` string
   (`resolve_all_types`, `append_inline_aliases`), enriches the manifest by
   alias-string join, uploads `bundled_types` + `type_manifest`.

At check time (any repo's CI, after downloading all `CloudRepoData`):

5. `recreate_type_files_and_check` writes one `<stem>_types.d.ts` per service
   (padded with `= unknown // carrick:missing-alias` for unresolved aliases),
   merges every repo's dependencies into one `package.json`, and runs
   `npm install --legacy-peer-deps` (node_modules and lockfile deleted first —
   genuinely uncached every run). `Analyzer::run_final_type_checking`
   (`src/analyzer/mod.rs:2025`) then spawns
   `npx ts-node ts_check/run-type-checking.ts`.
6. `ts_check` matches manifests, loads every `.d.ts` into one flat ts-morph
   project, gates unverifiable pairs, and calls `isAssignableTo` with
   per-protocol direction (HTTP/GraphQL: `producer ⊑ consumer`;
   socket/pubsub inverted; GraphQL producers get a structural envelope
   unwrap).
7. Verdicts come back as JSON keyed by human-readable endpoint labels, which
   Rust re-parses to join onto `CrossRepoMatch.type_compatible`
   (`parse_compat_endpoint` / `parse_producer_key`, plus `consumerLocation`
   since #260); mismatch detail is then re-serialized into a prose string the
   formatter regex-parses apart again for the PR comment.

### Failure classes this structure produces

- **Dangling-name decay.** Anything the expander can't inline (depth cap,
  unexpandable shapes, expander bugs) ships as a bare name, resolves to
  `any`/`unknown` at check time, and surfaces as "unverifiable" far from the
  cause. The guard stack in `compareTypes` exists because top types make
  `isAssignableTo` vacuously true (the `graphql|subscription|orderUpdated`
  false positive needed a *second* guard on the post-unwrap comparands).
- **Version-conflict soundness hole.** The merged install resolves conflicting
  majors arbitrarily, so one repo's types are checked against the wrong
  library version. `xrepo-corpus-1` deliberately ships a zod 3.x/4.x conflict
  (`payments-svc` `^4.0.0` vs siblings `^3.22.0`); the merged dependency map
  holds one `zod` entry, picked arbitrarily.
- **Semantic loss.** Eager structural expansion erases generic structure,
  nominal-ish semantics (branded types, classes with privates, enums,
  declaration merging), and type *names* — so even correct mismatch verdicts
  print anonymous `{ ... }` soup instead of "`OrderUpdate` requires `note`".
- **String-protocol coupling.** The marker comment, the alias-definition
  regexes duplicated in Rust (`dts_defines_alias` in `src/services/type_sidecar.rs:815`
  and `src/engine/mod.rs:2747`), socket direction labels, GraphQL kind casing,
  the verdict-label re-parse, and the analyzer→formatter mismatch-string
  round-trip must all stay byte-compatible across two languages.
- **CONFIRMED direction bug on HTTP request bodies.** `compareTypes` keys
  direction on protocol only (`ts_check/lib/type-checker.ts:387-408`); `type_kind` is used
  in the label but never in direction selection. Request pairs are built for
  non-GET/HEAD/OPTIONS endpoints (`src/engine/mod.rs:2109-2115`), `RequestBody`
  inference is wired end-to-end (`src/agents/file_orchestrator.rs:940-953`,
  `src/sidecar/src/type-inferrer.ts:690`), and the matcher pairs strictly per `type_kind`
  (`ts_check/lib/manifest-matcher.ts:732`). Data flows consumer → producer for request
  bodies (the caller sends the body the endpoint must accept), so the check
  should be `consumer ⊑ producer`; the code runs `producer ⊑ consumer`.
  Widening/narrowing verdicts on request bodies are inverted today. No
  fixture currently pins this — add one regardless of this migration.

## Target architecture

### Capture phase (sidecar, per service, at scan time)

**Output artifact per service: a types-only stub package**, replacing the
single `bundled_types` string:

```
@carrick/<service>/
├── package.json          # name, types entry, pinned deps (name@exact-version only)
├── tsconfig.snapshot.json
└── types/
    ├── surface.d.ts      # entry: export type Endpoint_<hash>_Response = ...
    └── **/*.d.ts         # compiler-emitted declaration tree (closure)
```

Generation:

1. Write a **surface entry `.ts` file inside the real repo project** — inside
   the effective `rootDir` (an entry at repo root with `rootDir: "src"` fails
   with `TS6059`) — that aliases each manifest anchor:
   - Explicit symbol: `export type Endpoint_abc_Response =
     import('./types/order').OrderResponse;`
   - Addressable handler (implicit): `export type Endpoint_def_Response =
     Awaited<ReturnType<typeof import('./routes/orders').getOrder>>;`
     (after machinery unwrap — see below). **Guards required before choosing
     this form** (all verified failure modes): the symbol must actually be
     exported (`checker.getExportsOfModule`), must not be an overload set
     (`ReturnType` silently resolves the *last* overload only), and must not
     be generic (type parameters erase to their constraint/`unknown`). Anchors
     failing these guards route to the node-builder path or are marked
     unverifiable-with-reason.
2. Run **the compiler's declaration emit** via
   `ts.createProgram([entry], parsedRepoOptions)` — the repo's own parsed
   tsconfig options, not CLI file roots (which ignore tsconfig entirely) —
   with `--noCheck --declaration --emitDeclarationOnly` (TS ≥ 5.5). Verified:
   `noCheck` emit exits 0 on repos with type errors and still fully computes
   inferred exported types, so **type-error-laden repos are not a blocker**.
   The compiler computes the transitive closure of local declarations,
   synthesizes local non-exported declarations where needed (it handles the
   unexported-`Secret`-interface case correctly — materially smarter than the
   node-builder API), and prints external references as real imports.
3. **Post-emit specifier rewrite pass (required component).** Declaration emit
   does *not* rewrite tsconfig-`paths`-mapped specifiers — `import { Item }
   from '@app/models/item'` ships verbatim and dangles after relocation into
   the stub (verified). `paths` cannot be fixed at check time either: it is
   program-global, and `@app/*`-style namespaces collide across stubs. The
   capture pass maps every emitted internal specifier through the producer
   repo's resolved `paths` to a tree-relative path. True relative specifiers
   survive relocation correctly (verified) and need no rewriting.
4. **Global augmentations are handled deliberately.** An entry-rooted program
   drops `declare global` / module-augmentation files outside the entry's
   import graph — and because emit proceeds despite errors, the tree ships
   with dangling global references unless caught. Capture must detect
   augmentations reachable from the closure's symbols and include those files
   in the emitted tree. (Their check-time interaction is handled in the
   checker — see below.)
5. **Anonymous inferred types** (inline handlers with no addressable symbol)
   print via the compiler's node builder (`checker.typeToTypeNode` with
   `NoTruncation`), which emits `import("pkg").T` / relative import-type
   references that resolve inside the shipped tree + pinned deps.
   **The node builder fails silently by default** (verified: unexported local
   interfaces, unique-symbol keys, and local recursive aliases print as
   dangling names with zero callbacks fired) — the fallback therefore requires
   a real `SymbolTracker` implementation performing `isSymbolAccessible`
   bookkeeping per tracked symbol; any inaccessible symbol demotes the alias
   to `structural_fallback`. `expandTypeStructural` survives only as that
   last-resort tier, and every alias records its tier in the manifest
   (`serialization: emitted | node_builder | structural_fallback`) so fidelity
   is measurable and ratchetable. Alongside `serialization`, every alias
   records how its anchor was produced: `anchor_origin: llm-symbol |
   deterministic-infer | anchor-backfill`. (Named `anchor_origin` because
   `provenance` is already taken by the op-level producer-provenance fields
   in `src/eval_output.rs`.) The two dimensions answer different questions:
   `anchor_origin` measures anchor recall (did we point at the right symbol
   at all), `serialization` measures capture fidelity (did the anchored
   symbol survive emission). A fidelity ratchet keyed on `serialization`
   alone would let an anchor-recall regression masquerade as an emit-tier
   improvement, and vice versa.
6. **Machinery unwrapping stays at capture time** where the real `Type` is in
   hand: transport generics (`Promise`, async iterables), agent-generated
   `ExtractionConfig` wrapper rules, and — moved here from the checker — the
   GraphQL resolver envelope unwrap. The checker never guesses at payload
   shape.
7. `package.json` dependencies are **pinned exact versions resolved from the
   repo's lockfile**, stored as `name@version` only (registry/proxy URLs
   stripped), pruned to specifiers actually referenced by the emitted tree.
8. **Capture-time self-check (new gate), keyed on diagnostics, not artifact
   existence** (emit succeeds even when poisoned): the stub package must
   typecheck standalone, with module resolution pointed at the source repo's
   `node_modules` when present. Classification is three-way:
   - **ok**: the alias resolves to a concrete type.
   - **allowlisted external**: resolution failed only through external
     specifiers whose package is pinned in the stub's `dependencies` and the
     checkout is bare (no `node_modules` at capture time). The alias is kept
     at its serialization tier with `capture_env: bare` and the failed
     specifier recorded; it is NOT downgraded. These specifiers resolve at
     check time against the stub's own installed pins, and the probe gates
     (`any`/`unknown`/`never`, both sides) remain the backstop for any alias
     that still decays there, so optimism here can produce unverifiable but
     never a false compatible. Known instance of the backstop firing: a type
     inferred *through* the missing library, such as `z.infer<typeof Schema>`
     where the schema const has no annotation; the const's inferred type
     bakes to `any` in the emitted tree, spike-verified as
     `export declare const StockAdjustSchema: any`.
   - **decayed**: a dangling internal specifier, an unpinned external, or a
     top-type resolution not explained by an allowlisted external failure
     downgrades the alias to `type_state: Unknown` *with a recorded reason*
     at capture time. The decay rule is thereby kept only for fully-internal
     closures and genuinely unexplained top types

   converting today's silent degradation (discovered at match time as a
   confusing "unverifiable") into a per-repo, per-alias capture error.
   Implementation note, spike-verified: the self-check program must set
   `skipLibCheck: false`; the stub tree is entirely `.d.ts`, and with
   `skipLibCheck: true` the checker skips declaration files wholesale,
   producing zero diagnostics and a vacuous gate (the same reason Check-phase
   step 7 mandates `skipLibCheck: false` for stub trees).
9. **JS-heavy services:** declaration emit from `allowJs` sources produces
   `any`-saturated declarations that will mass-fail the self-check. This is
   expected and honest — record it as capture degradation
   (`type_extraction_status` already exists for exactly this), and predict a
   near-zero surface-fidelity score for such services rather than discovering
   it in evals.

### Artifact shape and storage (requires coordinated carrick-cloud change)

The doc'd first instinct — `TypeSurface` as a file map inside `CloudRepoData` —
does not survive contact with the actual transport: `bundled_types` is
uploaded to S3 but the S3 object is **write-only dead weight**; the live read
path inlines the entire `CloudRepoData` of every service in the fleet into one
synchronous Lambda JSON response (`src/cloud_storage/aws_storage.rs:312,354`;
`download_all_repo_data` reads inline metadata and discards the per-repo
`s3_url` map — `src/engine/mod.rs:172` binds it as `_repo_s3_urls`). Sync Lambda
responses cap at ~6MB; declaration trees per service would blow it.

Therefore:

- **One content-addressed S3 object per surface**: gzip tarball of the tree,
  key = sha256 of contents. `CloudRepoData` carries only a descriptor:
  `{ surface_key, digest, byte_size, pinned_deps: BTreeMap<String,String>,
  tsconfig_snapshot, ts_version, artifact_version }` (~hundreds of bytes).
- `get-cross-repo-data` returns presigned GETs — the existing unused
  `AdjacentRepo.s3_url` plumbing is repurposed. The scanner downloads
  surfaces **lazily, only for repos that produced matched pairs**.
- Content addressing gives dedup for free: unchanged types across commits →
  same digest → skip upload (the existing check-or-upload hash dance already
  has this shape).
- **Join-time artifact version check:** a peer descriptor with a missing or
  older `artifact_version` is treated as having no surface — its pairs are
  unverifiable with reason `peer scanned with older Carrick — re-scan`. No
  compatibility shims; the fleet re-scans once after the release. (`cache_version`
  is checked only on the same-repo incremental path, `src/engine/mod.rs:797-815`;
  it does not and should not gate cross-repo joins.)
- This is a **wire-contract change in `carrick-cloud`** (upload flow at
  `src/cloud_storage/aws_storage.rs:342-368` branches on `bundled_types` today; the Lambdas
  must store descriptors and serve presigned surface GETs). The migration
  cannot land from this repo alone.

### Check phase (synthetic monorepo, at cross-repo analysis time)

Reuses `MonorepoBuilder`'s workspace shape (pnpm, `node-linker=isolated`,
per-stub `node_modules`), with these corrections:

1. **Workspace lives in scratch space** (`$RUNNER_TEMP`-rooted), never
   `.carrick/workspace` inside the scanned repo (pollutes the working tree,
   breaks `git diff`-based incremental detection, and risks being swept up by
   the scanner's own file discovery). Stub directory/package names are keyed
   on `service_name ?? repo_name` and sanitized exactly like
   `bundle_file_stems` (`src/engine/mod.rs:2534-2563`) — the current builder keys
   on raw `repoName`, re-creating both the monorepo-services-clobber bug and
   invalid `@carrick/org/repo` package names.
2. **pnpm is vendored as a devDependency of the sidecar** — version-pinned by
   the existing lockfile, installed by the `npm ci` step already in
   `action.yml`, invoked as `<sidecar>/node_modules/.bin/pnpm`. No corepack
   (deprecated; removed from Node 25 while the action pins Node 24 — a silent
   time bomb), no runtime tool download. **The silent npm fallback is
   deleted**: plain-npm flat installs physically duplicate packages and
   manufacture nominal false-incompatibles program-wide (see 6) while
   destroying the isolation guarantee — if pnpm is unavailable, the type pass
   fails with an explicit `isolation-unavailable` reason. Never trade
   soundness for availability silently.
3. **Per-stub install failure isolation.** A stub whose install fails (private
   registry dep of a peer, unpublished pin) degrades **only that repo's
   pairs** to unverifiable with the install error as the recorded reason —
   never fatal to the run (today's contract, `Err` at
   `src/engine/mod.rs:2858-2877`, would let one stale peer disable type checking
   for the whole fleet).
4. **Stub packages carry the declaration tree.** `types` points at
   `types/surface.d.ts`; internal relative imports resolve within the tree;
   external imports resolve against the stub's own pinned `node_modules`
   (verified: one `tsc` program genuinely loads two versions of the same
   package via per-stub `node_modules` walk-up and elaborates cross-version
   mismatches correctly). Checker tsconfig maps only the surfaces; use
   `moduleResolution: "bundler"` — the single program must accept both
   `.js`-suffixed specifiers (from `node16` producers) and extensionless ones.
5. **One probe file per matched pair**, named by a pair ID derived with the
   existing FNV-1a hasher (`src/type_manifest.rs` — a pair-level ID is new;
   the hasher is not). Probe content:

   ```ts
   // pair_<fnv-hash>.ts — <protocol> <method> <path> (<type_kind>)
   import type { Endpoint_abc_Response as Sent } from '@carrick/orders/surface';
   import type { Call_def_Response as Expected } from '@carrick/web/surface';

   type IsAny<T> = 0 extends 1 & T ? true : false;
   type IsUnknown<T> = unknown extends T ? (0 extends 1 & T ? false : true) : false;
   type IsNever<T> = [T] extends [never] ? true : false;
   type Not<T extends boolean> = T extends true ? false : true;
   type Assert<T extends true> = T;

   // Top/bottom-type gates, BOTH sides. IsAny alone is insufficient:
   // IsAny<unknown> is false; `Expected = unknown` yields zero diagnostics
   // (false compatible — the graphql|subscription|orderUpdated class), and
   // `Sent = never` likewise. All three gates, both sides, or the probe
   // reintroduces the exact hole this design exists to close.
   type _G1 = Assert<Not<IsAny<Sent>>>;
   type _G2 = Assert<Not<IsUnknown<Sent>>>;
   type _G3 = Assert<Not<IsNever<Sent>>>;
   type _G4 = Assert<Not<IsAny<Expected>>>;
   type _G5 = Assert<Not<IsUnknown<Expected>>>;
   type _G6 = Assert<Not<IsNever<Expected>>>;

   // Value-level assignability in the data-flow direction.
   declare const sent: Sent;
   const expected: Expected = sent;
   ```

   Value-level assignment, not `[X] extends [Y]` conditional types: the
   conditional-type relation diverges around `any` (resolves both branches),
   and the compiler's **elaborated errors** become the user-facing mismatch
   report. Verified equivalences: excess-property checks don't fire (variable,
   not fresh literal); weak-type rejection matches `isTypeAssignableTo`
   (parity with today's verdict function); `strictFunctionTypes` applies
   identically.
6. **Nominal-identity caveat (new false-positive class to manage).** Isolation
   makes verdicts nominal at package-copy granularity: two *byte-identical*
   copies of a class with private members fail cross-assignment ("separate
   declarations of a private property" — verified), so patch-level drift
   between repos (`bson@6.8.0` vs `6.8.1` `ObjectId`, `Decimal`, `Dayjs`)
   produces incompatible verdicts the merged install never did. Mitigation:
   dedupe semver-compatible pins across stubs (pnpm `overrides` keyed on
   compatible ranges) before install, so only genuinely conflicting majors
   remain physically duplicated — those *should* verdict incompatible.
7. **Verdict classification by diagnostic code + file, not line position
   alone**, with four buckets:
   - `TS2344` on a probe's gate lines → **unverifiable** (which side, which
     gate — `any`/`unknown`/`never` — is recoverable from the gate name);
   - `TS2322`/`TS2559`/`TS2739`/`TS2741`-class on the assignment → 
     **incompatible**; diagnostic text is the report;
   - errors on the probe's **import lines** (missing/renamed surface export) →
     **unverifiable** (third bucket the naive line-split misses);
   - **any diagnostic landing in a stub's own files poisons every probe
     touching that stub** (unverifiable), never reads as "no errors →
     compatible". This matters because cross-stub `declare global` collisions
     (`TS2717`) attribute to stub files, not probes (verified); with
     `skipLibCheck: true` they vanish and one stub's augmentation silently
     contaminates the other. Policy: `skipLibCheck: false` for stub trees so
     collisions surface, and the poisoning rule converts them to honest
     unverifiables. Checker also sets `noUnusedLocals: false` (else `TS6196`/
     `TS6133` land on gate/assignment lines and confuse classification).
8. **Direction is a table in the probe generator**, keyed on
   `(protocol, type_kind)` — one place, in Rust:

   | protocol | type_kind | sent | expected |
   |---|---|---|---|
   | http, graphql | response | producer | consumer |
   | http | request | consumer | producer |
   | socket, pubsub | both | consumer (emitter/publisher) | producer (listener/subscriber) |

   This structurally fixes the confirmed request-direction inversion and
   replaces the ~40-line direction comment in `compareTypes`.
9. **Unresolved anchors generate no probe.** The pair is recorded unverifiable
   in Rust, carrying the capture-time reason. No `= unknown` padding, no
   marker comments, no placeholder taxonomy.
10. **Diagnostic post-processing stays — upgraded, not deleted.**
    `cleanupPaths` cannot simply go: elaborated diagnostics print stub-absolute
    `import("/tmp/.../stubs/orders/node_modules/zed/index").ZedError` paths in
    exactly the most interesting error class (cross-stub conflicts), and the
    probe's `as Sent` aliasing puts `Sent`/`Expected` in the headline instead
    of real type names (nested elaboration lines keep real names). The scrub
    pass maps stub-absolute paths → `@carrick/<service>` labels and rewrites
    the headline aliases — strictly better output than today, but it is a
    component, not a deletion.
11. **Verdicts return keyed by pair ID as structured data** — verdict, bucket,
    scrubbed diagnostic, both anchors. `apply_compat_verdicts` joins by ID;
    `parse_compat_endpoint` / `parse_producer_key` are deleted, and the
    formatter consumes the structured payload instead of regex-parsing a
    prose mismatch string (`src/formatter/mod.rs:832-872` today).
12. **Protocol:** `build_workspace` cold installs will exceed the Rust
    client's 60s read deadline (`src/services/type_sidecar.rs:569,610`), and `execSync`
    blocks the sidecar's event loop so even `health` goes dark. The action
    spawns the install async and the sidecar emits progress/keepalive frames;
    the Rust client gets a workspace-scoped deadline. Workspace caching
    (pnpm store via `actions/cache`, keyed on the fleet pinned-deps hash) is
    a **same-repo re-run optimization only** — runners are ephemeral and
    caches are repo-scoped, so cross-repo warmth structurally cannot exist.
    Budget the cold path as the norm; benchmark on `xrepo-corpus-1/2` with a
    deliberately dep-heavy fixture **as a landing precondition with a
    number**, not a post-hoc measurement. Expect check-time memory well above
    the capture-phase ~100-200MB README figure: N isolated stubs parse N
    disjoint `@types`/lib graphs.

### Consumers that must be re-pointed (previously missed)

- **`resolve_per_endpoint_definitions` (`src/engine/mod.rs:1937`)** consumes
  `cloud_data.bundled_types` today to populate
  `resolved_definition`/`expanded_definition` on manifest entries — the
  payload of the MCP `get_endpoint_types` tool and a scored eval metric
  (`tests/eval_xrepo.rs:749-771`). Re-point it at the surface tree: resolve each
  alias in the stub package (a strictly richer source), keep emitting both
  the as-written and structural forms. The definition-fidelity metric is
  preserved, not deleted.
- **`src/signature_pass.rs` is unaffected** — it uses only the sidecar
  `infer` action, no `bundled_types`.

### What this deletes

- `src/sidecar/src/type-structural-expander.ts` as a primary path (last-resort tier only,
  measured); `src/sidecar/src/definition-resolver.ts`'s duplication of it.
- `append_missing_aliases`, `MISSING_ALIAS_MARKER` (both sides), both
  `dts_defines_alias` regex copies, placeholder-vs-real-`unknown`
  disambiguation (#244 machinery).
- The `any`/`unknown` imperative guard stack in `compareTypes` (replaced by
  declarative per-probe gates covering `any`/`unknown`/`never`).
- The merged `package.json` + `--legacy-peer-deps` install,
  `create_dynamic_tsconfig`'s `*-types` path mapping.
- `ts_check/lib/type-checker.ts`'s comparison core, the endpoint-label
  verdict round-trip in `src/analyzer/mod.rs`, and the analyzer→formatter
  mismatch-string regex round-trip.
- The deprecated-but-live `TypeBundler` and its `String.replace` renames.
- `MonorepoBuilder`'s silent npm fallback and its dead `isRelated`
  computation (`parseCheckResult` currently marks every check incompatible on
  any tsc failure — `src/sidecar/src/monorepo-builder.ts:661-673`).
- `CloudRepoData.bundled_types` and the S3 `types.d.ts` upload flow
  (replaced by the descriptor + content-addressed surface object; requires
  the coordinated `carrick-cloud` change).

### What stays unchanged

- LLM anchor extraction, SWC span plumbing, `ExtractionConfig` unwrap rules.
- The manifest, deterministic alias hashing (`src/type_manifest.rs`), and
  operation keys.
- The manifest matcher's normalization and per-protocol matching.
- The warm sidecar process model and stdio transport (new actions, async
  install handling per Check-phase 12).
- Verdict semantics for the scorer: `compatible / incompatible /
  unverifiable`, `None` never read as compatible.

## Known semantic limits (stated, not hidden)

- **Capture-time flags bake irreversibly.** With `strictNullChecks` off in
  the producer, `cond ? "a" : null` infers (and emits) `string`; no checker
  policy can recover the lost `null`. The tsconfig snapshot is diagnostic
  context, not mitigation.
- **`exactOptionalPropertyTypes` is not part of `strict`.** The checker
  compiles probes with `strict: true` and eOPT **off** (explicit decision:
  wire-level JSON cannot distinguish absent-tolerant from undefined-tolerant
  consumers reliably enough to justify the false-incompatible rate); the
  per-stub snapshot records each side's original setting for diagnosis.
- **Nominal drift** (Check-phase 6) is managed by semver-compatible dedupe,
  not eliminated: genuinely conflicting majors intentionally verdict
  incompatible.

## TypeScript version policy, and TypeScript 7 (ts-go)

Where the TS version materially matters, and where it doesn't:

- **Verdict semantics:** assignability is stable across versions; new
  strictness is opt-in. Version drift is not a verdict-accuracy risk in
  practice, but determinism demands **one pinned judge version for the whole
  fleet** (an accidental feature of today's force-pinned `typescript@5.8.3`
  that becomes an explicit policy: the check phase pins its own compiler and
  records it in results).
- **Capture emit:** the artifact records `ts_version`; the judge's parser
  must be ≥ the newest capture version (older-emitted `.d.ts` parse fine
  under newer compilers; the reverse can fail on new syntax).
- **Deprecation clock:** `baseUrl` is deprecated in TS 6 and gone in TS 7,
  which retires today's `create_dynamic_tsconfig` gymnastics on its own
  schedule and reinforces the post-emit specifier-rewrite pass as the
  correct home for path-alias handling.

**TypeScript 7 (the Go-native compiler) is an argument *for* this design,
and the adoption line falls exactly on the v2 architecture's seam.** As of
the 7.0 RC (June 2026), `typescript@rc`'s `tsc` *is* the native compiler
(~10× faster, shared-memory parallelism), but the stable programmatic API
slips to 7.1+ — and ts-morph, which wraps the JS ("Strada") compiler API, is
fully broken on 7.0. That splits cleanly:

- **Check phase: TS7-ready by construction.** v2 deliberately reduces the
  judge to "run `tsc --noEmit` over real files and classify diagnostics by
  file + code" — no compiler API at all. Swapping the judge to the native
  `tsc` is a version bump, and it lands the speedup on precisely the flagged
  landing precondition (cold-check wall-clock over N isolated stub programs).
  As a native binary it also removes the check phase's Node/npx dependency.
  Spike must verify: diagnostic-code parity for the classifier's buckets, and
  behavior on the probe/gate patterns.
- **Capture phase: primary tier is TS7-ready; fallback tier is not.** The
  `emitted` tier is a CLI-shaped declaration emit of a generated entry — no
  API — but TS7's declaration emit intentionally differs from TS6's
  (isolated-declarations-leaning); the corpus spike decides whether capture
  emits on 7 or stays on 6 initially (6-emitted trees check fine under a 7
  judge, so the phases can adopt independently). The `node_builder` fallback,
  span→node anchor location, and the inferrer's machinery unwrapping are
  genuine compiler-API work: they stay on ts-morph/TS6 (side-by-side via the
  `@typescript/typescript6` compat package if the sidecar also carries 7)
  until the stable 7.1 API, then migrate.
- **The deeper alignment:** the current system cannot ride ts-go at all —
  both the sidecar's extraction and `ts_check`'s `isAssignableTo` live inside
  the JS compiler API. v2's philosophy (shift heavy lifting from API calls
  into "generate real files, let the compiler batch-process them") is exactly
  what makes the native compiler adoptable incrementally, and every anchor
  the capture phase moves from `node_builder` to `emitted` shrinks the
  remaining API dependence.

Policy: do not gate the migration on TS7. Pin the judge, measure the spike on
both 6.x and the 7.0 RC, take the faster one that passes diagnostic-parity,
and revisit the sidecar's API usage at 7.1.

Freshness note (2026-07-18): TypeScript 7.0 went GA on 2026-07-08 and remains
CLI-only until the stable programmatic API lands in 7.1+, so the split above
(capture on 6, judge free to move to 7) stands unchanged. One earlier worry is
retired: typescript-go issue #972 (declaration emit failing under type errors,
which would have broken the `--noCheck` capture emit) was fixed in May 2025
and is not a blocker.

## Future hook: wire-truth vs type-truth

TS assignability is a proxy for what Carrick actually claims: **wire
compatibility of JSON payloads**. `Date` (producer) vs `string` (consumer) is
wire-compatible but TS-incompatible; branded types are wire-identical but
TS-distinct. Because v2 checks run in real tsc probes, a type-level
serialization model becomes possible:

```ts
const expected: Serialize<Expected> = serializedSent; // Serialize<T> models JSON.stringify
```

where `Serialize<T>` maps `Date → string`, respects `toJSON()`, and drops
functions/`undefined`. Impossible to bolt onto `isAssignableTo` over
pre-serialized text; a natural follow-up experiment once probes are the
substrate. It would also dissolve the nominal-drift class (Check-phase 6) for
serializable payloads. Not in scope for the initial migration.

## Eval leverage

The migration decomposes today's hardest-to-debug metric (verdict accuracy)
into independently measurable stages:

1. **Anchor resolution rate** (exists): manifest entries reaching
   `Explicit`/`Implicit`.
2. **Surface fidelity (new):** stub self-check pass rate + per-alias
   `serialization` histogram (`emitted` > `node_builder` >
   `structural_fallback`), reported split by `anchor_origin` so anchor-recall
   loss and serialization loss ratchet independently. Ratchet in the Tier-A
   baseline. Note: this is a
   *new* metric with a different denominator than resolution rate — an alias
   can be `Explicit` today *and* decay to `any` at check time, so today's
   resolution rate overstates today's fidelity; do not treat the two as
   comparable gates.
3. **Match F1** (exists, unchanged).
4. **Verdict accuracy** (exists): now attributable — a wrong verdict with a
   clean stage-2 is a checker bug; with a dirty stage-2, a capture bug.
5. **Definition fidelity** (exists): re-pointed at the surface tree, not
   deleted.

Honesty note: `tests/eval_tier_a.rs` and `tests/eval_xrepo.rs` are **report-only
monitors** (their own headers say so), not merge gates. For this migration
specifically, the landing PR asserts the xrepo verdict rows against a
checked-in expected baseline so "no new false-compatibles" is mechanical, not
a human eyeballing a `workflow_dispatch` run.

New eval cases to add with the migration: HTTP request-body direction
(widening and narrowing, both orders — pins the confirmed bug independent of
the migration); `unknown`- and `never`-decayed sides (pins the probe gates);
a `declare global` / module-augmentation producer (pins closure inclusion and
the stub-poisoning rule); a tsconfig-`paths`-aliased producer (pins the
specifier rewrite); patch-level dependency drift on a private-member class
(pins semver dedupe); conflicting majors where the *types* differ (upgrade
the zod 3/4 fixture from dependency-report-only to a type-verdict assertion);
a `Date`-serialization pair (expected incompatible today; flips when
`Serialize<T>` lands).

## Migration plan

**One release, not three.** An intermediate release that captures the new
artifact but still checks the old way (or vice versa) leaves the fleet with a
checker whose input no longer exists. Per the no-parallel-paths policy, the
new capture, the workspace checker, and the deletion of the old path ship in
the same change. Since there are no users, there is no compatibility window
to manage — old artifacts are ignored via the join-time `artifact_version`
check and the fleet re-scans once.

The incremental development happens **pre-merge in the offline harness**:
`LocalDirStorage` two-phase runs the entire pipeline against
`xrepo-corpus-1/2` with zero fleet exposure. Sequence of work (all on one
branch, eval-checked at each step, merged together):

1. Capture: surface entry generation with the anchor guards, `noCheck`
   declaration-emit tree, specifier rewrite pass, augmentation inclusion,
   `SymbolTracker`-backed node-builder fallback, self-check gate,
   `serialization` tagging. Measure surface fidelity on the corpus.
2. Check: workspace build in scratch space (vendored pnpm, sanitized
   service-keyed stubs, semver dedupe overrides), probe generation with the
   full gate set and the `(protocol, type_kind)` direction table,
   diagnostic-code classification with stub-poisoning, scrub pass, ID-keyed
   structured verdicts. Measure verdict accuracy vs the checked-in baseline.
3. Re-point `resolve_per_endpoint_definitions` at the surface tree; swap the
   formatter to structured verdict payloads; delete the old path.
4. Coordinated `carrick-cloud` change (descriptor storage, presigned surface
   GETs, content-addressed objects) lands first on the cloud side — it is a
   prerequisite, and the plan explicitly budgets it rather than pretending
   this is scanner-only work.
5. Release; trigger main-branch re-scans of indexed repos (cloud-side
   nudge) so the fleet converges without waiting for organic pushes.

Landing preconditions, each with a number attached: surface fidelity and
verdict accuracy vs the checked-in corpus baseline; cold-install wall-clock
and peak RSS on the dep-heavy fixture.

## Review record (2026-07)

The proposal was probed by three adversarial passes before landing; material
findings and their disposition:

- **Compiler mechanics** (claims tested against a real tsc): the two
  load-bearing bets — tsc as serializer, tsc as judge — **held**, and
  `--noCheck` declaration emit strengthened the capture story. Refuted as
  originally drafted and fixed above: `IsAny`-only probe gates missed
  `unknown`/`never` (reintroducing the design's own motivating false-positive
  class); global augmentations broke both closure emit and filename-only
  diagnostic attribution; `paths`-mapped specifiers ship verbatim and dangle
  (rewrite pass now required); `cleanupPaths` moved from the delete list to
  an upgraded scrub component; per-stub isolation's nominal false-positives
  on identical/patch-drifted classes (semver dedupe added); the
  `ReturnType<typeof import(...)>` form's unexported/overload/generic failure
  modes (capture guards added); silent node-builder failures
  (`SymbolTracker` now required).
- **Codebase fact-check**: every file/line claim verified; the HTTP
  request-direction bug **confirmed real and reachable**; found hidden
  consumers the delete list missed — `resolve_per_endpoint_definitions` /
  MCP `get_endpoint_types` and the definition-fidelity eval metric (now
  re-pointed, not deleted), the `aws_storage` upload contract, and the
  analyzer→formatter mismatch-string round-trip (now replaced with
  structured verdicts).
- **Migration/ops**: the artifact rides the inline Lambda metadata blob, not
  S3 (storage redesigned to content-addressed objects + lazy fetch); the
  original 3-step migration left a checker-less shipped state (collapsed to
  one release + offline pre-work); `cache_version` doesn't gate cross-repo
  joins (explicit `artifact_version` added); no pnpm exists in the composite
  action and corepack is a Node-25 time bomb (vendored via sidecar
  devDependencies; silent npm fallback deleted); workspace relocated to
  scratch space; per-stub install-failure isolation; async install protocol
  for the stdio transport; the cited eval "gates" are report-only monitors
  (baseline assertion added for the landing PR).
