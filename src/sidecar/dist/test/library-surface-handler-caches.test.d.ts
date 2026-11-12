/**
 * carrick#2977 (caching half of ticket A): `list_library_surface` with
 * `handlers: true` prints each type once per program and builds a handler
 * parameter's members once per package, across every specifier of that
 * package in one request. Neither cache may move a byte of any listing.
 *
 * Every package below is invented and its declarations are hand-written.
 */
export {};
