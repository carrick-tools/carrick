/**
 * Partially skipped declaration emit — the corpus-2 notifications-svc
 * collapse.
 *
 * `program.emit` sets `emitSkipped` per-PROGRAM even when only one file's
 * declaration emit failed: an ambient `declare module "fastify"` stub with
 * `export =` and no `FastifyInstance` export makes `export default app`
 * unnameable (TS4023), that one file's .d.ts is skipped, and every other
 * file's .d.ts is still written to the emit callback. The old wholesale
 * `if (emitSkipped) fail` threw the entire emitted tree away, so the whole
 * service's types went unresolved and its pub/sub edges collapsed to None.
 *
 * Pins:
 *  1. capture keeps the emitted subset: success, healthy aliases fully
 *     resolved, the tree contains their .d.ts files;
 *  2. an alias whose module produced no .d.ts is demoted to
 *     structural_fallback with a capture_failure_reason, and its surface
 *     line is rewritten to `unknown` (no dangling specifier that would
 *     smear healthy aliases at self-check or poison the service at check);
 *  3. the total-failure path is retained: nothing emitted at all is still
 *     the wholesale 'declaration emit was skipped' fail;
 *  4. fail-closed at check: a demoted producer alias verdicts unverifiable
 *     (IsUnknown probe gate), never compatible, while a healthy alias from
 *     the SAME partially-emitted capture still probes compatible.
 */
export {};
