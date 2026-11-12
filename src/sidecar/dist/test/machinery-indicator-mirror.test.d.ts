/**
 * carrick#371 mirror-drift guard.
 *
 * The machinery-indicator set is intentionally DUPLICATED across the capture
 * seam: `type-inferrer.ts` (ts-morph, the v1 abstain path) and
 * `capture/machinery.ts` (raw `ts`, the capture demote path) each carry their
 * own copy, because the seam forbids sharing a module across it — a capture/
 * file may import only node builtins + `typescript` + its own bundle, and the
 * rest of the sidecar may reach the bundle only via `api.js`/`index.js`
 * (enforced by capture-v2-seam.test.ts). De-duplicating into a shared module
 * would break that boundary, so the two copies are kept in lockstep by THIS
 * test instead: if they drift, one detection path silently stops abstaining and
 * the carrick#371 false verdict can reappear on whichever path lost a member.
 */
export {};
