/**
 * carrick#1564: `verify_client_semantics` checks each library-semantics claim
 * against the package's own type declarations.
 *
 * Every package below is invented and its declarations are hand-written, so
 * the tests pin the predicates rather than one library's current types. The
 * two answered clients mirror the contract sample: a callable instance built
 * by `create({ baseURL })` that takes a config object, and one built by
 * `create({ prefixUrl })` that takes `(path, options)`. A third package
 * declares one member per failure, and the rest cover what cannot be read.
 */
export {};
