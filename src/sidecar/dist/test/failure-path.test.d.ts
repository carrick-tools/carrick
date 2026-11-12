/**
 * Unit table for `reachedOnlyOnFailure` (carrick#1796): which reads the source
 * reaches only after a response failed.
 *
 * Each case is a function body with one `READ()` marker where a body read
 * would sit. `res` is the response the walk tracks; `other` is a value it does
 * not. A case is decided `true` only when no status in 200-299 can reach the
 * marker, so every `false` row below is a read the walk must keep.
 */
export {};
