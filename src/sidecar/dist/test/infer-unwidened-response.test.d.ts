/**
 * carrick#1516: a response inference carries, beside the type the compiler
 * infers, the type its handler actually returns with no literal widened.
 *
 * One fixture, several shapes that differ in what matters:
 *
 * - a decorated controller method returning a service call, whose service
 *   maps rows to objects with a conditional string and a negative numeric
 *   choice (the literal sits two calls away from the route). The same service
 *   method carries every literal the reading must leave alone: a quoted
 *   property name, a literal type in an annotation, a tagged template, and a
 *   parameter default the controller overrides. Marking any of them adds a
 *   diagnostic and costs the whole reading;
 * - a boolean discriminant across two returns;
 * - an object built with a literal and then assigned another value, which the
 *   reading must NOT narrow (the compiler reports the assignment once the
 *   literal is kept, and the reading is dropped);
 * - a handler whose `let` bindings take literals directly, through a
 *   conditional, and through `||` inside parentheses, and are then reassigned:
 *   none of them is marked (marking one adds a diagnostic and costs the
 *   reading), while the `const` beside them is;
 * - a service method with a DECLARED return type, which is the contract and is
 *   not followed: its body would cost the reading a diagnostic if it were;
 * - a callback route whose payload is located by span and by expression text,
 *   after a marked literal, so the re-read has to find the payload again in
 *   the rewritten file.
 */
export {};
