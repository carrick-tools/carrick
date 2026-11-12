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
import { Node, SyntaxKind, ts, type SourceFile } from 'ts-morph';
/** One node of the file, with what the lookups read of it. */
export interface IndexedNode {
    node: ts.Node;
    kind: SyntaxKind;
    /** `getStart()`: the first character of its own text. */
    start: number;
    /** `getEnd()`. */
    end: number;
    /** `getStartLineNumber()`. */
    line: number;
    /** Its place in `getDescendants()` order. */
    order: number;
}
/** How many file indexes have been built: tests read it to prove reuse. */
export declare function nodeIndexBuilds(): number;
/** The ts-morph node for an indexed one. */
export declare function wrap<T extends Node = Node>(sourceFile: SourceFile, entry: IndexedNode): T;
/** Every descendant, in `getDescendants()` order. */
export declare function descendantsOf(sourceFile: SourceFile): readonly IndexedNode[];
/**
 * The call expressions, in `getDescendantsOfKind(SyntaxKind.CallExpression)`
 * order.
 */
export declare function callsOf(sourceFile: SourceFile): readonly IndexedNode[];
/** The descendants that start on `line`, in walk order. */
export declare function descendantsOnLine(sourceFile: SourceFile, line: number): readonly IndexedNode[];
/**
 * The descendants that start on a line from `first` to `last`, in walk
 * order. Walk order is not line order (a doc comment starts before the node
 * that owns it but follows it in the walk), so the lines' lists are merged
 * back into walk order.
 */
export declare function descendantsOnLines(sourceFile: SourceFile, first: number, last: number): IndexedNode[];
