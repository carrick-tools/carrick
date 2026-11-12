/**
 * Regression for carrick#766: a line-only anchor that lands on a statement
 * which declares no function was answered by the NEXT function declaration in
 * the file.
 *
 * `findFunctionByLine` accepted any function whose declaration starts within
 * two lines of the anchor, in either direction. A route whose handlers are
 * re-exported at the bottom of the file
 *
 *     export { entry, read };            // the anchor line
 *
 *     function toRouteFilters(raw) { … } // two lines later
 *
 * therefore reported the private helper's return type as the endpoint's
 * response contract. Silently: nothing about the answer says it came from a
 * neighbouring declaration, so a helper with a clean return type reads as the
 * contract with no signal at all. A confidently wrong contract is worse for a
 * consumer check than the honest `unknown`, which is what this now answers.
 *
 * The tolerance itself is load-bearing — an anchor often names the line of a
 * registration or a binding whose handler starts a line or two in — so it is
 * kept, with the one rule that makes it safe: a candidate starting AFTER the
 * anchor line is rejected when a statement begins at or after the anchor line
 * and does not contain it. Nothing but trivia may sit between the anchor and
 * the function it is taken to name.
 *
 * Resolving the exported binding to its registration's handler (which would
 * answer `{ id: string }` here rather than abstaining) needs to know WHICH of
 * the two exported bindings the operation is — the export names both an action
 * and a loader — and the infer request does not carry that. Tracked separately;
 * this change is the half that stops the false contract.
 */
export {};
