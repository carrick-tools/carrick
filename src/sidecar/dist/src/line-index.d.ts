import type { Node } from 'ts-morph';
/**
 * The 1-based line of a position in a text, counted by line feeds alone: one
 * more than the `\n`s before the position. That is the count ts-morph's
 * `getStartLineNumber` makes. A lone carriage return or a Unicode line
 * separator ends a line for the compiler and not here.
 *
 * The text is read once; each lookup after that is a binary search.
 */
export declare function lineIndex(text: string): (pos: number) => number;
/**
 * `node.getStartLineNumber()` from a per-file index. ts-morph counts the
 * file's line feeds from position 0 on every call, so a walk that asks of
 * every node of a file pays the file's size once per node (carrick#1935).
 */
export declare function startLineOf(node: Node): number;
/** `node.getEndLineNumber()` from the same per-file index. */
export declare function endLineOf(node: Node): number;
