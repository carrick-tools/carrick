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
import { lineIndex } from './line-index.js';

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

interface FileIndex {
  /** Every descendant, in `getDescendants()` order. */
  all: IndexedNode[];
  /** The call expressions among them, in the same order. */
  calls: IndexedNode[];
  /** Descendants by the line they start on, each list in walk order. */
  byLine: Map<number, IndexedNode[]>;
}

/**
 * The two ts-morph internals the index stands on (ts-morph is pinned
 * exactly): the compiler-node walk `getDescendants()` wraps, and the factory
 * lookup that wraps one compiler node.
 */
interface NodeInternals {
  _getCompilerDescendantsIterator(): Iterable<ts.Node>;
  _getNodeFromCompilerNode(node: ts.Node): Node;
}

const internals = (sourceFile: SourceFile): NodeInternals =>
  sourceFile as unknown as NodeInternals;

const indexes = new WeakMap<ts.SourceFile, FileIndex>();

let builds = 0;
/** How many file indexes have been built: tests read it to prove reuse. */
export function nodeIndexBuilds(): number {
  return builds;
}

function indexOf(sourceFile: SourceFile): FileIndex {
  const key = sourceFile.compilerNode;
  let index = indexes.get(key);
  if (!index) {
    index = buildIndex(sourceFile);
    indexes.set(key, index);
  }
  return index;
}

function buildIndex(sourceFile: SourceFile): FileIndex {
  builds += 1;
  const file = sourceFile.compilerNode;
  const lineAt = lineIndex(file.text);
  const all: IndexedNode[] = [];
  const calls: IndexedNode[] = [];
  const byLine = new Map<number, IndexedNode[]>();
  for (const node of internals(sourceFile)._getCompilerDescendantsIterator()) {
    const start = node.getStart(file);
    const entry: IndexedNode = {
      node,
      kind: node.kind,
      start,
      end: node.end,
      line: lineAt(start),
      order: all.length,
    };
    all.push(entry);
    if (entry.kind === SyntaxKind.CallExpression) calls.push(entry);
    const onLine = byLine.get(entry.line);
    if (onLine) onLine.push(entry);
    else byLine.set(entry.line, [entry]);
  }
  return { all, calls, byLine };
}

/** The ts-morph node for an indexed one. */
export function wrap<T extends Node = Node>(sourceFile: SourceFile, entry: IndexedNode): T {
  return internals(sourceFile)._getNodeFromCompilerNode(entry.node) as T;
}

/** Every descendant, in `getDescendants()` order. */
export function descendantsOf(sourceFile: SourceFile): readonly IndexedNode[] {
  return indexOf(sourceFile).all;
}

/**
 * The call expressions, in `getDescendantsOfKind(SyntaxKind.CallExpression)`
 * order.
 */
export function callsOf(sourceFile: SourceFile): readonly IndexedNode[] {
  return indexOf(sourceFile).calls;
}

/** The descendants that start on `line`, in walk order. */
export function descendantsOnLine(sourceFile: SourceFile, line: number): readonly IndexedNode[] {
  return indexOf(sourceFile).byLine.get(line) ?? [];
}

/**
 * The descendants that start on a line from `first` to `last`, in walk
 * order. Walk order is not line order (a doc comment starts before the node
 * that owns it but follows it in the walk), so the lines' lists are merged
 * back into walk order.
 */
export function descendantsOnLines(
  sourceFile: SourceFile,
  first: number,
  last: number
): IndexedNode[] {
  const { byLine } = indexOf(sourceFile);
  const found: IndexedNode[] = [];
  for (let line = Math.ceil(first); line <= last; line++) {
    const onLine = byLine.get(line);
    if (onLine) found.push(...onLine);
  }
  return found.sort((a, b) => a.order - b.order);
}
