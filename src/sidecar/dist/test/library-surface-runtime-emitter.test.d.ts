/**
 * carrick#3032 (K3-A): the event names Node's runtime gives every
 * `EventEmitter` (`error`, `newListener`, `removeListener`) are a fixed
 * table, keyed to the runtime types package's own declaration of the emitter.
 * The lister marks the members that declaration owns (`runtime_events`), left
 * out of `surface_sha256`, and the verifier accepts a `reserved` claim for the
 * names from the same table, on those members only.
 *
 * Every package below is invented and its declarations are hand-written, in
 * the two layouts the runtime types package has used: `Key<K, T>` name slots
 * (`fixtures/runtime-emitter/`) and `EventNames<T, E>` (`layout-25/`).
 */
export {};
