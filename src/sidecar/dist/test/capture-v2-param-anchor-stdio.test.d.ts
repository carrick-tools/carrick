/**
 * A capture anchor's `param_name` arrives at the capture when the request
 * comes through the sidecar process (carrick#1980).
 *
 * A subscriber's contract is what its handler receives, so its anchor names a
 * parameter and the capture resolves that parameter before anything else
 * (carrick#498). The scanner never calls the capture in process: it writes a
 * `capture_v2` request to the sidecar's stdin, where the request schema reads
 * it. The schema did not declare `param_name` on an `infer` anchor and drops
 * what it does not declare, so every such anchor lost its parameter name
 * before the capture saw it and fell to the expression locator, which types
 * whatever expression starts on the anchor's line: the callback itself, or a
 * comparison inside the handler. Both self-check clean, and so does the
 * callback of an untyped parameter, which is a function where the parameter
 * is a top type the check would not have judged.
 *
 * Every test that named a parameter called `captureStub` directly, where no
 * schema stands between the request and the capture. This one sends the
 * request the way the Rust client serialises it (the `kind` tag first, the
 * fields in declaration order, an absent option omitted) to a sidecar process
 * and reads what that process wrote.
 *
 * In each fixture the parameter is not the first expression on the anchor's
 * line, so the two readings cannot agree by accident.
 *
 * Fixtures are synthetic and generically named.
 */
export {};
