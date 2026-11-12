/**
 * v2 capture ("tsc as the serializer") — bare-checkout stdio test.
 *
 * Drives the sidecar over the same JSON-over-stdio wire the Rust client uses
 * (the integration point for src/services/type_sidecar.rs), against the
 * xrepo-corpus-3 inventory-svc fixture, which has NO node_modules — the bare
 * checkout case from the v2 design study.
 *
 * What this pins:
 *  1. `tsc --noCheck --declaration --emitDeclarationOnly` capture succeeds on
 *     a bare checkout (exit clean, tree emitted).
 *  2. Third-party references survive as syntactic import references
 *     (`import { z } from "zod"` + `z.infer<typeof StockAdjustSchema>`
 *     verbatim in the emitted tree) where expandTypeStructural bakes `any`.
 *  3. In-repo ambient declaration sources (the fixture's stubs.d.ts with
 *     `declare module "zod"` — the corpus determinism recipe) ship verbatim:
 *     tsc never re-emits input .d.ts, so without the copy the tree's
 *     references dangle. With them shipped the stub is self-contained and
 *     the zod-derived alias self-checks ok even on a bare checkout.
 *  4. External deps referenced by the tree pin to exact lockfile versions,
 *     pruned to what the tree actually references.
 *  5. anchor_origin (amendment 2) rides every alias record, and the
 *     per-capture fidelity metric is emitted.
 *
 * The amendment-1 allowlist path (pinned external, unresolved on bare, NO
 * ambient stub) is pinned by capture-v2-forms.test.ts on a purpose-built
 * fixture; this corpus fixture stubs its externals so it cannot exercise it.
 */
export {};
