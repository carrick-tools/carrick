/**
 * Seam enforcement for the v2 capture bundle (pinned decision 11a: "seam,
 * not split" — one cleanly bounded bundle, the stdio action surface is the
 * only contract, zero cross-boundary imports outside it).
 *
 * Mechanically pins:
 *  1. Modules inside src/capture/ import only node builtins, `typescript`,
 *     and each other — never ts-morph, never the legacy sidecar modules.
 *  2. Modules outside the bundle reach it only through the two sanctioned
 *     doors: types from capture/api.js (types.ts) and captureStub from
 *     capture/index.js (the dispatcher).
 */
export {};
