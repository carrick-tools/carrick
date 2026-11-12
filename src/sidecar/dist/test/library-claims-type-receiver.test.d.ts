/**
 * carrick#2977 ticket B: the verifier reads a claim placed `of: "type"` on a
 * type export, the receiver `type` the surface lister lists, built at its
 * declared type-parameter defaults (`declare const v: Reply;`). A route
 * handler's reply is often a type-only export, so its `send` can now be a
 * label. `yieldsOutcome` is unchanged (decision 11): its tests here pin what
 * it already verifies.
 *
 * Every package below is invented and its declarations are hand-written.
 */
export {};
