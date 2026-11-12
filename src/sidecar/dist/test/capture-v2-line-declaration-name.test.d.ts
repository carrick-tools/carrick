/**
 * carrick#1785: the capture's line fallback never serves a declaration's own
 * type.
 *
 * When an infer anchor's span and expression text name nothing (or it carries
 * a line alone), `locateNode` falls back to the first expression on the line.
 * `ts.isExpression` is true for every identifier, including a declaration's
 * NAME, which a pre-order walk reaches before the declared body. So on a line
 * that declares a type alias, an interface, a class or a function, the
 * fallback read the declared entity's name and the capture published that
 * entity's whole type. That is how carrick#1775 served a whole GraphQL
 * operation under a field-keyed row.
 *
 * A line that declares something names the declaration, not a payload: the
 * anchor abstains (`unknown`, the reason on `self_check_detail`, nothing for
 * the backfill to re-anchor). It does not walk on to the next node either: on
 * a function's line that is the first parameter, a second wrong answer that
 * self-checks clean.
 *
 * A binding whose name IS the value read at that position (a destructured
 * element, a shorthand property) is not a declaration in this sense and keeps
 * resolving.
 */
export {};
