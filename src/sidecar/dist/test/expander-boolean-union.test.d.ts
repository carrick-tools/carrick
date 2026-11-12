/**
 * A boolean prints as `boolean` wherever a served type is rendered
 * (carrick#1165).
 *
 * The compiler stores `boolean` as the union `false | true`. Its own printer
 * folds the pair back, but the structural expander renders union members one
 * by one: an optional `flag?: boolean` has its `undefined` stripped and the two
 * literals left over were joined as `flag?: false | true`. That text is what
 * the index serves, and the capture's literal backfill carries it into the
 * stub. The compiler's print can hold the same pair once a union is rebuilt
 * from members, so the text-level canonicaliser folds it too.
 */
export {};
