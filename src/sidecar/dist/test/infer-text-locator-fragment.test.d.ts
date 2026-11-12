/**
 * carrick#2056: a text locator whose text differs from the source was
 * answered by a node inside what the text names.
 *
 * Text matching tries the exact text, then a node containing it, then a node
 * the text contains. That last, reverse rule exists for drift AROUND the true
 * node (a `return` or an `await` the source does not have there, #335). Where
 * the true node's own text drifted (other quotes, a dropped statement), it
 * bound a fragment of it instead:
 *
 *  - a call written with other quotes was answered by the `new
 *    URLSearchParams(…)` inside its URL argument, so a consumer was typed as
 *    the query string it builds;
 *  - a returned object whose nested callback the text paraphrased was answered
 *    by the object that callback returns, so a route published one item of an
 *    inner list.
 *
 * Three rules, none keyed on a name:
 *
 *  - a node the locator's own parse places strictly inside the expression it
 *    names is never bound by the reverse rule;
 *  - a string literal reads the same in any quotes, on both sides;
 *  - a call-result locator that finds no call publishes nothing, rather than
 *    whatever other node its text overlaps.
 */
export {};
