/**
 * carrick#2054: a response union whose members a request field chooses.
 *
 * A handler that answers `{ x }` for `?mode=a` and `{ y }` otherwise
 * publishes `{ x } | { y }`, and a call that states `mode=a` and reads `y`
 * is judged against the whole union. The sidecar reads, beside the published
 * union, which request field picks each member (`response_modes`): the
 * request reads the handler's branches test, and for one placed field with
 * every test read, the union each stated value receives.
 *
 * The published inference does not change: every row's `type_string` is
 * pinned to the text the inference published before this reading existed.
 *
 * Every name below is a placeholder; the shapes are the ones a handler on
 * the platform `Request`/`Response` (default lib) or on a library's request
 * object takes.
 */
export {};
