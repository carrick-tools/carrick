/**
 * The capture's deep walk names EVERY position a top type sits at
 * (carrick#1752).
 *
 * `any` and `unknown` are intrinsic types: the compiler hands out one object
 * for each, however many members are typed with it. The walk's cycle set
 * remembers types it has met, so it used to report the first `unknown` and
 * the first `any` it reached and pass over every later position silently.
 *
 * That hid a cause. A member whose import the scanned checkout cannot resolve
 * is rewritten to `unknown` in the emitted declaration (carrick#1397), and the
 * record labels it `unresolved_import`. A member the source itself declares
 * `unknown` is labelled `declared`. When the declared one came first in walk
 * order, the rewritten one was never reported, and the record read as if every
 * top type in the type were the author's own. The scanner now trusts that
 * record to decide whether an open member leaves a typed contract, so the
 * record has to name every position.
 *
 * The first finding is unchanged by this: it is still the first position in
 * walk order, and it is what the check phase pre-gates on.
 *
 * An OBJECT type met twice (one interface typing two members) is still walked
 * once: its members are one declaration, so whatever cause a top type inside
 * it has, the first path already names it.
 */
export {};
