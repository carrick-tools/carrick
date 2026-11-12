/**
 * Every node of a source file, walked once per file (carrick#1935).
 *
 * The inferrer locates each item by a line, a text near a line, or a span.
 * Each of those lookups used to call `sourceFile.getDescendants()`, which
 * walks and wraps every node of the file, once per item: on a generated
 * client of 8,000+ lines that was 71% of an infer request's time. Here the
 * file is walked once, the positions are read once, and a lookup wraps only
 * the nodes it returns.
 *
 * The walk is ts-morph's own (`getDescendants()` iterates the same compiler
 * nodes in the same order, tokens, syntax lists and its comment nodes
 * included), and a node is wrapped through the project's own factory, so a
 * lookup returns the very wrapper the walk returned.
 *
 * The index belongs to the compiler's source file node, not to the file's
 * name: replacing a file's text gives it a new node, so a file rewritten for
 * one reading and then restored is indexed again each time.
 */
import { SyntaxKind } from 'ts-morph';
import { lineIndex } from './line-index.js';
const internals = (sourceFile) => sourceFile;
const indexes = new WeakMap();
let builds = 0;
/** How many file indexes have been built: tests read it to prove reuse. */
export function nodeIndexBuilds() {
    return builds;
}
function indexOf(sourceFile) {
    const key = sourceFile.compilerNode;
    let index = indexes.get(key);
    if (!index) {
        index = buildIndex(sourceFile);
        indexes.set(key, index);
    }
    return index;
}
function buildIndex(sourceFile) {
    builds += 1;
    const file = sourceFile.compilerNode;
    const lineAt = lineIndex(file.text);
    const all = [];
    const calls = [];
    const byLine = new Map();
    for (const node of internals(sourceFile)._getCompilerDescendantsIterator()) {
        const start = node.getStart(file);
        const entry = {
            node,
            kind: node.kind,
            start,
            end: node.end,
            line: lineAt(start),
            order: all.length,
        };
        all.push(entry);
        if (entry.kind === SyntaxKind.CallExpression)
            calls.push(entry);
        const onLine = byLine.get(entry.line);
        if (onLine)
            onLine.push(entry);
        else
            byLine.set(entry.line, [entry]);
    }
    return { all, calls, byLine };
}
/** The ts-morph node for an indexed one. */
export function wrap(sourceFile, entry) {
    return internals(sourceFile)._getNodeFromCompilerNode(entry.node);
}
/** Every descendant, in `getDescendants()` order. */
export function descendantsOf(sourceFile) {
    return indexOf(sourceFile).all;
}
/**
 * The call expressions, in `getDescendantsOfKind(SyntaxKind.CallExpression)`
 * order.
 */
export function callsOf(sourceFile) {
    return indexOf(sourceFile).calls;
}
/** The descendants that start on `line`, in walk order. */
export function descendantsOnLine(sourceFile, line) {
    return indexOf(sourceFile).byLine.get(line) ?? [];
}
/**
 * The descendants that start on a line from `first` to `last`, in walk
 * order. Walk order is not line order (a doc comment starts before the node
 * that owns it but follows it in the walk), so the lines' lists are merged
 * back into walk order.
 */
export function descendantsOnLines(sourceFile, first, last) {
    const { byLine } = indexOf(sourceFile);
    const found = [];
    for (let line = Math.ceil(first); line <= last; line++) {
        const onLine = byLine.get(line);
        if (onLine)
            found.push(...onLine);
    }
    return found.sort((a, b) => a.order - b.order);
}
