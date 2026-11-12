/**
 * carrick-tools/carrick-cloud#1118: the field-level report behind the mismatch
 * text.
 *
 * The end-to-end sentences are pinned in check-v2.test.ts against the real
 * pnpm + tsc pipeline. These cases drive the walk directly against a probes
 * package on disk — no pnpm, no network — because what is under test is the
 * one property the text cannot show: that the walk NEVER contradicts the judge.
 * It names a field only where the compiler's own relation says the members do
 * not assign, and it stays silent on the shapes where per-member assignability
 * and whole-type assignability legitimately diverge.
 */
export {};
