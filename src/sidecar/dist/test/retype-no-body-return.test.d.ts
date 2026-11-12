/**
 * carrick#2743: a consumer call returned whole from a function that declares
 * it returns no body (`Promise<void>`, `Promise<undefined>`, or a response
 * object of `void`) states no body to judge.
 *
 * The clients below are hand-written and synthetic. Both verbs leave the
 * result's type parameter `R` free: only the result mentions it, so in
 * source the compiler takes it from the return position (`void`), and the
 * file compiles. The retype writes the producer's body as the call's first
 * type argument; the rest then take their defaults instead of being
 * inferred, and the declared return reports the default response object.
 * That diagnostic is about the written argument, not a body anyone reads.
 *
 * `plain` defaults `R` to the response object; `cond` reads it through a
 * conditional alias on a unique-symbol default, as newer clients do.
 */
export {};
