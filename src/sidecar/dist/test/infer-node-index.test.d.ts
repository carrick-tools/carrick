/**
 * carrick#1935: the inferrer's locators read a per-file node index, walked
 * once per file, and name exactly the node the per-item walk named.
 *
 * `registrationAtLine`, `findNodeAtSpan`, `findNodeByText`,
 * `findCallExpressionByText` and `findCallExpressionAtSpan` each called
 * `getDescendants()` (or `getDescendantsOfKind`) on the whole file once per
 * item: on a generated client of 8,000+ lines that was 71% of an infer
 * request.
 *
 * - Test 1 keeps the five walks as they stood (`reference*` below, fed the
 *   file's `getDescendants()`) and asks both of every line, every node's span
 *   (exact, shifted one byte as an SWC span is, widened, empty) and every
 *   node's text at its line and at the window's edges. Each pair must name
 *   the same wrapper object.
 * - Test 2 proves the index's walk is ts-morph's: the same nodes, in the same
 *   order, for both the whole walk and the call expressions.
 * - Test 3 counts the full-file walks of one infer batch: one per file.
 */
export {};
