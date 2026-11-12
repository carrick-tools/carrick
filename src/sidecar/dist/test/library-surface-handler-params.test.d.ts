/**
 * carrick#2674 S3: `list_library_surface` with `handlers: true` (carrick#2966)
 * lists the members a route handler's own parameters declare, so a picker can
 * name `request.body` on a receive op's `request` field. They have a cap of their own, so the main listing, its
 * `truncated` count and the hash are the ones the lister gave without them.
 *
 * Every package below is invented and its declarations are hand-written.
 */
export {};
