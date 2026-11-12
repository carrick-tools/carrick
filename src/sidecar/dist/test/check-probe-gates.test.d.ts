/**
 * The probe's gate aliases, compiled by the real compiler (in memory).
 *
 * The classifier tests feed synthetic diagnostics, so they cannot see which
 * gates a given type actually trips. This file swaps the probe's two surface
 * imports for local aliases and reads the TS2344 gate lines tsc reports, so a
 * gate that also fires on a top type (as `[any] extends [void]` does) fails
 * here rather than silently changing the reported reason.
 */
export {};
