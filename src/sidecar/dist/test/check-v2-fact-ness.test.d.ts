/**
 * carrick#707 R1d: the check phase's fact-ness walk.
 *
 * The probe gates prove neither side is WHOLLY `any`/`unknown`/`never`. They
 * cannot see a member three levels down, and `any` there is bidirectionally
 * assignable — so a producer declaring `{ id: string; meta: any }` clears every
 * gate and then reads compatible against a consumer expecting anything at all.
 * That is the absence of a comparison, not the result of one.
 *
 * The capture-time walk catches most of it, but by design not the case where a
 * member decays through a pinned external: at capture time that is
 * TypeScript's `error` placeholder, excluded there because installing the pin
 * heals it (carrick#450). This walk runs on the assembled workspace AFTER that
 * install, so it sees what the capture could not.
 *
 * These cases drive the module directly against a probes package on disk — no
 * pnpm, no network — because what is under test is whether the walk resolves
 * the probe's two imported surface aliases through real declaration files and
 * reports what it finds. The end-to-end wiring is covered by the check-phase
 * families in capture-v2-deep-any.test.ts.
 */
export {};
