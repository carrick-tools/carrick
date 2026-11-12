/**
 * carrick#2472 S7 (blueprint row T7): proof for the replacement of the deleted
 * sibling-call rule.
 *
 * A file-based handler returns `serialise(payload, { status })`. The old rule
 * promoted the callee to "serialiser" from a returned sibling call that stated
 * a status (deleted in #3019). The replacement is the compiler: when the
 * callee has a resolvable declaration returning `Response`, the return is
 * transport machinery and the walk reads the handler's returned argument, so
 * the payload type is the declared one, not `any`.
 *
 * The control handler is byte-identical except that its callee comes from a
 * module with no declaration. It stays at the honest `any`
 * (`no_payload_evidence`). Together they show the result comes from the
 * declaration alone, not from the sibling call.
 */
export {};
