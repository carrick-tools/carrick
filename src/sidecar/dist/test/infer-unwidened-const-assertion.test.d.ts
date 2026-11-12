/**
 * carrick#2343: a literal the source already asserts `as const` is not marked
 * again by the unwidened reading (carrick#1516).
 *
 * Marking it again writes `('warn' as const) as const`, which the compiler
 * rejects (a const assertion applies to a literal only). The reading drops on
 * any diagnostic it adds, so one such literal anywhere on a handler's path
 * cost the whole reading, and the check had nothing to read a second time
 * (carrick#1538).
 *
 * The shapes: a const-asserted literal inside an element of a list the
 * handler returns, beside a discriminant that widens; the same assertion
 * through parentheses and in angle-bracket form; a const-asserted list; and an
 * assertion to a wider type, which still takes a marked literal.
 */
export {};
