/**
 * Long requests say they are alive (carrick#1914).
 *
 * The scanner's deadline for a request is how long the sidecar may stay
 * silent about it. `infer` can hold thousands of requests in one batch, so it
 * writes a `progress` frame as it finishes them; `bundle` and
 * `verify_library_claims` write one when the program they read is ready,
 * which is the part of their work whose size the request does not bound.
 *
 * `retype_check` rebuilds the program for each item it judges, so it writes
 * a frame as it finishes them, as `infer` does (carrick#1945).
 *
 * A capture is four pieces of compiler work, each as large as the service
 * and none bounded by the request, so `capture_v2` writes a frame as it
 * reaches each stage and as it resolves anchors (carrick#1916). The frames
 * are also what is left of a capture whose process dies: the stage it had
 * reached.
 *
 * Two layers are pinned:
 * - the inferrer and the capture tell their caller as they get past each
 *   unit, between units and not at the end, whatever the unit answered;
 * - over stdio, those requests write `progress` frames before their terminal
 *   frame, under their own request id, and never after it.
 *
 * The repo is written to the OS temp dir. Nothing here is timed: pacing is
 * tested with a clock in `progress.test.ts`, and that a frame written by a
 * blocked handler leaves the process at once is tested from the reader's side
 * in the scanner's `sidecar_operation_timeout_test.rs`.
 */
export {};
