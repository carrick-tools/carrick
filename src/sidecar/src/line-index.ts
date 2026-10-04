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
