import type { Node } from 'ts-morph';

/**
 * The 1-based line of a position in a text, counted by line feeds alone: one
 * more than the `\n`s before the position. That is the count ts-morph's
 * `getStartLineNumber` makes. A lone carriage return or a Unicode line
 * separator ends a line for the compiler and not here.
 *
 * The text is read once; each lookup after that is a binary search.
 */
export function lineIndex(text: string): (pos: number) => number {
  const starts = [0];
  for (let i = 0; i < text.length; i++) if (text[i] === '\n') starts.push(i + 1);
  return (pos) => {
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid] <= pos) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  };
}

const fileLineIndexes = new WeakMap<object, (pos: number) => number>();

function lineAtIn(node: Node, pos: number): number {
  // Keyed on the compiler node: a manipulation (`replaceWithText`, a retype)
  // gives the file a new one, so the index is built again from the new text.
  const key = node.getSourceFile().compilerNode;
  let lineAt = fileLineIndexes.get(key);
  if (!lineAt) {
    lineAt = lineIndex(key.text);
    fileLineIndexes.set(key, lineAt);
  }
  return lineAt(pos);
}

/**
 * `node.getStartLineNumber()` from a per-file index. ts-morph counts the
 * file's line feeds from position 0 on every call, so a walk that asks of
 * every node of a file pays the file's size once per node (carrick#1935).
 */
export function startLineOf(node: Node): number {
  return lineAtIn(node, node.getStart());
}

/** `node.getEndLineNumber()` from the same per-file index. */
export function endLineOf(node: Node): number {
  return lineAtIn(node, node.getEnd());
}
