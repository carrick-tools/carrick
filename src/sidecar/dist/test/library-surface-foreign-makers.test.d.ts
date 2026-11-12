/**
 * carrick#3745: a package of many values typed by another package's generic
 * builder lists the builder's member receivers on every value, which runs the
 * lister out of heap. Past a request-wide estimate (member makers times member
 * count over such values) those values list only their `export` receiver, and
 * the surface says how many makers it left out. At or under it, the listing is
 * today's. The sidecar runs at the library builder's child heap (1630 MB).
 * Both packages are invented (see schema-values-fixture.ts).
 */
export {};
