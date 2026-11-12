/**
 * carrick#1446: a capture record reads clean for an alias whose own type does
 * not resolve in the emitted tree.
 *
 * A literal anchor's text names types as they read where v1 printed it. A
 * name its source module declares but does not export cannot be imported into
 * the surface, so the surface names it bare and the compiler reads that
 * position as its unresolved-reference placeholder: `any`, printed as the
 * name. The program declares the name, so `undeclared_names` stays empty; the
 * deep walk leaves the placeholder out on purpose (a pinned external heals at
 * check time); the record said `self_check: ok` with nothing else, and every
 * reader of the record counted the type clean.
 *
 * The record now lists, in `unresolved_in_tree`, every position at which the
 * alias's type holds that placeholder in the emitted tree: `''` for the alias's
 * own type, member paths otherwise. It is a list of its own, not part of
 * `any_provenance`, because the check phase pre-gates on `any_provenance[0]`
 * and this record is about what the index can publish, not about a verdict.
 */
export {};
