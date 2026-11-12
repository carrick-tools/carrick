/**
 * carrick#1935: the text locator asks a file's lines from an index, and the
 * index names exactly the node the per-node lookup named.
 *
 * `findNodeByText` walked every node of a file and asked each for its start
 * line, which ts-morph computes by counting the file's line feeds from
 * position 0. On a generated client of several thousand lines that is the
 * file's size once per node, per request.
 *
 * - Test 1 counts `getStartLineNumber` calls (no clock) for text-located
 *   requests on a generated SDK. It fails on the walk.
 * - Test 2 keeps the walk as it stood (`referenceMatch`: `matchByText` with
 *   ts-morph's own line) and asks both of every call text and a sample of
 *   node texts, in generated and real files, line endings included.
 * - Test 3 pins `startLineOf` / `endLineOf` to ts-morph for every node, and
 *   shows the index is rebuilt after the file is rewritten.
 */
export {};
