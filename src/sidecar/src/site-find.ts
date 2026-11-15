/**
 * `find_site`: the capture's own site finder, asked by a caller that held a
 * value's span in one text and needs its node in another (a held check whose
 * bounds an edit crossed). The value is found by its parent slot, never by its
 * text: the node of the old span sits in a slot of its parent (the n-th child
 * of a call, a `return`'s expression, an arrow's body), and the answer is the
 * node in that slot of the new text nearest the old position carried through
 * the edit, so an edit to the value's own text, or the same text elsewhere
 * (another handler's send, a binding of the same name), cannot move it. A call's
 * slot also needs the callee to read as before unless the edit reaches the call.
 * A node must be one the capture reads a value type at (`isPreferredTarget`).
 * No program is loaded and nothing is read from disk: both texts are the
 * caller's.
 */

import { ts } from 'ts-morph';
import { isPreferredTarget } from './capture/index.js';

export type SiteAnswer =
  | { start: number; end: number }
  | { reason: 'value_not_a_node' | 'not_wrappable' };

type Slot = { parentKind: ts.SyntaxKind; index: number; head: string | undefined };

/** The text a call's callee reads as, without whitespace. */
function headOf(parent: ts.Node, sourceFile: ts.SourceFile): string | undefined {
  if (ts.isCallExpression(parent) || ts.isNewExpression(parent)) return parent.expression.getText(sourceFile).replace(/\s+/g, '');
  return undefined;
}

function slotOf(node: ts.Node, sourceFile: ts.SourceFile): Slot | undefined {
  const parent = node.parent;
  if (!parent) return undefined;
  let index = -1;
  let at = 0;
  parent.forEachChild((child) => {
    if (child === node) index = at;
    at += 1;
  });
  return index < 0 ? undefined : { parentKind: parent.kind, index, head: headOf(parent, sourceFile) };
}

/** The outermost value-space node whose span is exactly `[start, end)`. */
function nodeAtSpan(sourceFile: ts.SourceFile, start: number, end: number): ts.Node | undefined {
  let found: ts.Node | undefined;
  const visit = (node: ts.Node): void => {
    if (found || node.getEnd() < end || node.getStart(sourceFile) > start) return;
    if (node.getStart(sourceFile) === start && node.getEnd() === end && ts.isExpression(node)) {
      found = node;
      return;
    }
    node.forEachChild(visit);
  };
  visit(sourceFile);
  return found;
}

/** The one region the edit changed, the common prefix and suffix cut off (the caller's `regionOfEdit`). */
function editRegion(oldText: string, newText: string): { oldStart: number; oldEnd: number; newStart: number; newEnd: number } {
  const limit = Math.min(oldText.length, newText.length);
  let prefix = 0;
  while (prefix < limit && oldText.charCodeAt(prefix) === newText.charCodeAt(prefix)) prefix += 1;
  let suffix = 0;
  while (suffix < limit - prefix && oldText.charCodeAt(oldText.length - 1 - suffix) === newText.charCodeAt(newText.length - 1 - suffix)) suffix += 1;
  return { oldStart: prefix, oldEnd: oldText.length - suffix, newStart: prefix, newEnd: newText.length - suffix };
}

/** Where `position` of the old text lands in the new: shifted past the edit, kept before it, scaled inside it. */
function carried(position: number, region: ReturnType<typeof editRegion>): number {
  if (position <= region.oldStart) return position;
  if (position >= region.oldEnd) return position + (region.newEnd - region.newStart) - (region.oldEnd - region.oldStart);
  return Math.min(region.newEnd, region.newStart + (position - region.oldStart));
}

export function findSite(request: {
  file: string;
  oldText: string;
  newText: string;
  start: number;
  end: number;
}): SiteAnswer {
  const oldFile = ts.createSourceFile(request.file, request.oldText, ts.ScriptTarget.Latest, true);
  const sourceFile = ts.createSourceFile(request.file, request.newText, ts.ScriptTarget.Latest, true);
  const held = nodeAtSpan(oldFile, request.start, request.end);
  const slot = held && slotOf(held, oldFile);
  if (!slot) return { reason: 'value_not_a_node' };
  const region = editRegion(request.oldText, request.newText);
  const target = carried(request.start, region);
  let best: ts.Node | undefined;
  let bestDistance = Infinity;
  const visit = (node: ts.Node): void => {
    const parent = node.parent;
    if (parent && parent.kind === slot.parentKind && isPreferredTarget(node) && ts.isExpression(node)) {
      const at = slotOf(node, sourceFile);
      if (at && at.index === slot.index) {
        const start = node.getStart(sourceFile);
        // The callee is how the slot is told from its namesakes; an edit that reaches the call may have changed it.
        const reached = parent.getEnd() > region.newStart && parent.getStart(sourceFile) < region.newEnd;
        if (at.head === slot.head || reached) {
          const distance = Math.abs(start - target);
          if (distance < bestDistance) {
            best = node;
            bestDistance = distance;
          }
        }
      }
    }
    node.forEachChild(visit);
  };
  visit(sourceFile);
  const node = best;
  if (!node) return { reason: 'value_not_a_node' };
  // A call cannot wrap a spread, or the name a shorthand property is also the value of.
  if (ts.isSpreadElement(node) || (ts.isIdentifier(node) && node.parent && ts.isShorthandPropertyAssignment(node.parent))) {
    return { reason: 'not_wrappable' };
  }
  return { start: node.getStart(sourceFile), end: node.getEnd() };
}
