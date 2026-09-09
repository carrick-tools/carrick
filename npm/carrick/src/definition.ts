// Cross-repo go to definition: from a call site to the producer handler in
// another repo, and from a route to the consumers that call it (carrick#881).
//
// This is the one jump nothing else in the editor can make, and the whole risk
// is answering a question that was not about Carrick. Four rules hold it down,
// and every one of them is a test in test/definition.test.ts.
//
// **Fall through, never hijack.** A position outside every row's span returns
// nothing at all, so the client's other definition providers answer as they did
// before Carrick was installed. A row states an anchor, `line` plus `col`, and
// no end: the span used here therefore runs from the anchor to the end of that
// line. The anchor column is a fact, so a position left of it is not the row;
// end of line is the honest upper bound, and taking the whole line instead
// would claim the `const user =` in front of a call.
//
// `col` is a 1-based column the index recorded. Where it counts bytes and the
// line holds multi-byte text, this span starts a little right of the anchor and
// the answer is empty rather than wrong, which is the direction to fail in.
//
// **Never a guessed path.** A counterpart is `repo` plus `file`, stated
// absolutely by the payload, and it is offered only when that path exists on
// this machine. Nothing here searches for a repo by service name or tries the
// workspace root: a counterpart this machine cannot point at yields no
// location, not a best-effort one. `resolveCounterpart` in diagnostics.ts is
// the one place that rule lives.
//
// The switch is `carrick.definition`, in surfaces.ts with the other off
// switches; off means an empty answer here rather than a withdrawn capability.
//
// **A candidate may answer, but never alone beside a fact.** A jump was asked
// for and a wrong one costs a keystroke back, so the model's reading may answer
// where nothing else does. Where a fact row matches the same position, the
// fact's locations come first and the candidate's follow.
//
// Known deviation from the design record, which says the peek title labels a
// candidate: LSP's `Location` and `LocationLink` carry no title, and the peek
// renders the target file and line only. There is nowhere to put the label, so
// ordering is what is enforced. (Local mode indexes no candidate rows at all
// today, so this governs future rows rather than current behaviour.)

import { isCandidate, type CheckItem, type CheckResult } from "./contract.ts";
import { resolveCounterpart, type Range } from "./diagnostics.ts";
import { pathToFileURL } from "node:url";

/** An LSP `Location`. `Location[]` is the answer shape; `[]` is "no answer". */
export type Location = { uri: string; range: Range };

export type DefinitionOptions = {
  /** Injectable for tests; the real one hits the disk. */
  exists?: (target: string) => boolean;
};

/** LSP positions are 0-based; the payload's line and column are 1-based. */
export type Position = { line: number; character: number };

/**
 * True when the position sits in the row's span: on its line, at or right of
 * its anchor column. A row with no line covers nothing, because a row that
 * cannot say where it is cannot claim a position.
 */
export function coversPosition(item: CheckItem, position: Position): boolean {
  if (typeof item.line !== "number") return false;
  if (position.line !== item.line - 1) return false;
  const startCharacter = Math.max(0, (item.col ?? 1) - 1);
  return position.character >= startCharacter;
}

function locationsFor(item: CheckItem, exists?: (target: string) => boolean): Location[] {
  const locations: Location[] = [];
  for (const counterpart of item.counterparts ?? []) {
    const resolved = resolveCounterpart(counterpart, exists);
    if (!resolved) continue;
    const line = Math.max(0, (counterpart.line ?? 1) - 1);
    locations.push({
      uri: pathToFileURL(resolved).toString(),
      // A counterpart states a line and no column, so the range is the start of
      // that line: an editor reveals the line, and a guessed column would only
      // put the cursor in the wrong place.
      range: { start: { line, character: 0 }, end: { line, character: 0 } },
    });
  }
  return locations;
}

/**
 * Every counterpart of every row covering `position`, deduplicated, facts
 * first.
 *
 * Both directions fall out of one rule: a row's counterparts are its other
 * side, so a call jumps to the producer and a route jumps to its consumers,
 * and several locations is a valid answer the editor renders as a picker.
 */
export function definitionsAt(
  result: CheckResult,
  position: Position,
  options: DefinitionOptions = {},
): Location[] {
  if (result.error) return [];
  const covering = (result.items ?? []).filter((item) => coversPosition(item, position));
  const facts = covering.filter((item) => !isCandidate(item));
  const candidates = covering.filter((item) => isCandidate(item));
  const seen = new Set<string>();
  const locations: Location[] = [];
  for (const item of [...facts, ...candidates]) {
    for (const location of locationsFor(item, options.exists)) {
      const key = `${location.uri}:${location.range.start.line}`;
      if (seen.has(key)) continue;
      seen.add(key);
      locations.push(location);
    }
  }
  return locations;
}

/** The position out of a `textDocument/definition` request, or null. */
export function positionOf(params: Record<string, unknown> | undefined): Position | null {
  const position = params?.["position"] as { line?: unknown; character?: unknown } | undefined;
  if (typeof position?.line !== "number" || typeof position?.character !== "number") return null;
  return { line: position.line, character: position.character };
}
