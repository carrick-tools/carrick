/**
 * carrick#2041: the literal backfill runs inside the first capture.
 *
 * When a capture demotes an alias (its module's declaration emit was skipped,
 * TS4023) and the scanner holds a usable literal text for it, the stub is
 * captured a second time with that alias re-anchored as a literal at
 * `anchor-backfill` origin. That second capture used to be a second
 * `capture_v2` request in a fresh process: a second program over the service,
 * a second anchor pass, a second emit and a second self-check. One request
 * now carries the texts and answers both stubs, and both must be exactly what
 * the two requests wrote: every file of both stub packages, byte for byte, and
 * every alias record.
 *
 * The reference is the two-request path, run here as the scanner ran it: a
 * capture, then a capture of the anchors the scanner's `backfill_anchors`
 * built from its records.
 */
export {};
