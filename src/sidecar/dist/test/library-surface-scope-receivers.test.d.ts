/**
 * carrick#2774 T1: a member of an instance receiver whose call returns another
 * object that routes are registered on lists that object as `scope_receiver`
 * (receiver id `R>scope:m`). It has a cap of its own and is left out of
 * `surface_sha256`, so a listing without it, `truncated` and the hash are the
 * ones the lister gave before.
 *
 * Every package below is invented and its declarations are hand-written.
 * `fixtures/library-surface-scope-receivers.golden.json` holds what the lister
 * of the commit before this change gave for the same package: the hash, and
 * the listing under a tiny `max_entries`.
 */
export {};
