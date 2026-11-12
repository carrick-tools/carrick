/**
 * carrick#3696: an instance receiver's own property that has no call
 * signature, and whose type has callable members, is listed in the
 * receiver's `properties` with those members, the way a procedure router
 * hangs its procedure builder off the made instance (`t.procedure.query`).
 * A claim reaches it as an op's `path: [name]`, so the receiver grammar is
 * unchanged. It has a budget of its own, counted in `properties_left_out`,
 * and is left out of `surface_sha256`, so the listing without it,
 * `truncated`, the handler type ids and the hash are the ones the lister
 * gave before.
 *
 * Every package below is invented and its declarations are hand-written.
 * `fixtures/library-surface-receiver-properties.golden.json` holds what the
 * lister of the commit before this change gave for the same package: the
 * hash, the listing with handlers, and the listing under a tiny
 * `max_entries`.
 */
export {};
