/**
 * carrick#1915: the function a line names comes from a per-file index, and
 * the index answers exactly what the walk it replaced answered.
 *
 * `findFunctionByLine` used to walk every node of the file on every call and
 * ask every statement for its start line, which ts-morph computes by counting
 * the file's newlines up to the node. A signature pass asks once per
 * unannotated slot, so a file cost its size times its slots: 81% of the pass
 * on a 2,345-file program.
 *
 * The walk is kept here, and only here, as the reference: `walkReference` is
 * its loop as it stood, fed what the walk recomputed on every call (the
 * functions and statements among the file's descendants, and ts-morph's own
 * start line for each), so its answer is unchanged. Every line of every file
 * below is asked of both, and the two must name the same node.
 *
 * The files: shapes written for the rules (ties, nesting, the forward window,
 * every statement kind, line endings), and every TypeScript file of the
 * sidecar's own source and tests, which is 200-odd files of real code.
 */
export {};
