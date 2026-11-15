/**
 * `find_site`: the capture's own site finder, asked by a caller that held a
 * value's span in one text and needs its node in another (a held check whose
 * bounds an edit crossed). The old span's text is the locator; it runs
 * `nodeByExpressionText`, the exact-text match the capture binds a locator
 * with, over the new text's syntax, from the line the old span started on.
 * No program is loaded and nothing is read from disk: both texts are the
 * caller's.
 */

import { ts } from 'ts-morph';
import { nodeByExpressionText } from './capture/index.js';

export type SiteAnswer =
  | { start: number; end: number }
  | { reason: 'value_not_a_node' | 'not_wrappable' };

export function findSite(request: {
  file: string;
  oldText: string;
  newText: string;
  start: number;
  end: number;
}): SiteAnswer {
  const wanted = request.oldText.slice(request.start, request.end);
  const sourceFile = ts.createSourceFile(request.file, request.newText, ts.ScriptTarget.Latest, true);
  const oldFile = ts.createSourceFile(request.file, request.oldText, ts.ScriptTarget.Latest, true);
  const fromLine = oldFile.getLineAndCharacterOfPosition(request.start).line + 1;
  const node = nodeByExpressionText(sourceFile, wanted, fromLine);
  if (!node || !ts.isExpression(node)) return { reason: 'value_not_a_node' };
  // A call cannot wrap a spread, or the name a shorthand property is also the value of.
  if (ts.isSpreadElement(node) || (ts.isIdentifier(node) && node.parent && ts.isShorthandPropertyAssignment(node.parent))) {
    return { reason: 'not_wrappable' };
  }
  return { start: node.getStart(sourceFile), end: node.getEnd() };
}
