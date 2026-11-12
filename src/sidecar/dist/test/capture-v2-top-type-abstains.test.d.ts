/**
 * The capture must abstain when its infer anchor resolves a bare top type
 * (carrick#766).
 *
 * `finishInferAnchor` reads the type at the located node, tries the #433
 * inline-literal recovery when that type is a whole top type, and then — until
 * this guard — printed whatever it held through the node builder. When the
 * recovery could not help, what it printed was `any`, and the alias's surface
 * line read `export type <alias> = any;`.
 *
 * That line is the contract the index publishes. `any` there says "a type was
 * inferred and it collapsed"; the truth is "the locator pointed at a node the
 * checker could tell us nothing about". `unknown` — no contract stated here —
 * is the honest word, and the reason travels on `capture_failure_reason`.
 *
 * Live shape behind this: a route whose handlers are built by a framework
 * factory and re-exported at the bottom of the file. Both operations anchor at
 * the export line; carrick#771 stopped the v1 walk answering with the
 * neighbouring helper, so the alias falls to its own capture infer anchor,
 * which resolved the export statement — type `any` — and published it.
 *
 * The export line itself now abstains one step earlier: a line whose first
 * node is a declaration's name (here an export specifier) names no payload
 * (carrick#1785). This guard keeps its own subject on the factory call that
 * built the handler, whose binding decays through the same missing package.
 */
export {};
