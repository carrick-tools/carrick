/**
 * carrick#1341: the channel for a statement that is not a mismatch.
 *
 * Two things a compat check can now find are true of a pair it called
 * COMPATIBLE, so neither can ride on `diagnostic`, which exists only beside a
 * mismatch: an optionality gap (the sender always provides a field the
 * receiver declares optional — legal, assigns, and still a drift between two
 * sources), and the note that the comparison was made against the JSON wire
 * form rather than the declared one (carrick#1340), which is why a producer
 * `Date` read as a `string` is not reported.
 *
 * Two halves are under test and they fail for different reasons:
 *   - the walk really finds both statements on pairs the compiler's own
 *     relation calls compatible (driven against a probes package on disk, no
 *     pnpm, no network, same harness as check-v2-field-report.test.ts);
 *   - `classifyPair` puts them on `notes` for EVERY bucket, and a note never
 *     moves `bucket`, `resolved` or `unresolved_reason`.
 */
export {};
