/**
 * Union member order must not depend on the order the checker created the
 * member types (carrick#735).
 *
 * A union's constituents are stored sorted by type id, and a literal type is
 * created — and interned — the first time a declaration mentioning it is
 * checked. So on a tree where two declarations spell the same literal set in
 * different orders, whichever one is reached first decides how BOTH print.
 * That is what made two runs of the same binary over an unchanged tree write
 * two different `expanded_definition` strings for one operation.
 *
 * The perturbation here is exactly the live one: the same source, resolved in
 * two different alias orders. Before the fix the two orders print two
 * different strings; after it they are byte-equal.
 */
export {};
