/**
 * The canonical union order must hold inside the COMPILER'S OWN PRINT
 * (carrick#775, #760).
 *
 * `expandTypeStructural` orders every union it walks, and #778 made the depth
 * backstop do the same. Neither reaches the unions that never get walked at
 * all: the walk hands a whole subtree to `namedText` when the type is a
 * library type kept by name, when an object has no named properties to
 * descend into (an index signature alone), and at tuples, functions and
 * cycles. Everything inside those prints stays in type-id order, which is
 * creation order.
 *
 * That is what the 0.3.45 index shows: 12 of the largest service's 40 literal
 * unions still printed in creation order after #778, and every one of them sits
 * inside a compiler print of one of these two shapes — a library generic's
 * instantiation, or an index-signature body.
 *
 * The perturbation is the instrument from `expander-union-order.test.ts`: one
 * source, two aliases spelling the same literal set in different orders,
 * resolved in both orders over two fresh programs. Whichever declaration the
 * checker reaches first interns the literals and so decides the printed order
 * of BOTH — unless the print is normalised.
 */
export {};
