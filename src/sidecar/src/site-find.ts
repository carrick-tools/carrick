/**
 * `find_site`: the capture's own site finder, asked by a caller that holds
 * a value's expression text and needs its node again (a held check whose
 * bounds an edit crossed). It runs `nodeByExpressionText`, the exact-text
 * match the capture binds a locator with, over the file's syntax: no program
 * is loaded, and the file's text is either the caller's own (an unsaved
 * edit) or the disk's.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import { ts } from 'ts-morph';
import { nodeByExpressionText } from './capture/anchors.js';
import type { FindSiteResponse } from './types.js';

/** The file a request names, resolved under the repo root; null when it lies outside it. */
function underRepo(repoRoot: string, file: string): string | null {
  const resolved = path.resolve(repoRoot, file);
  const relative = path.relative(repoRoot, resolved);
  return relative.startsWith('..') || path.isAbsolute(relative) ? null : resolved;
}

export function findSite(
  repoRoot: string,
  request: { file: string; text: string; from_line?: number; file_text?: string }
): Omit<FindSiteResponse, 'request_id' | 'status'> {
  const resolved = underRepo(repoRoot, request.file);
  if (!resolved) throw new Error(`find_site: ${request.file} is outside the repo`);
  const source = request.file_text ?? fs.readFileSync(resolved, 'utf8');
  const sourceFile = ts.createSourceFile(resolved, source, ts.ScriptTarget.Latest, true);
  const node = nodeByExpressionText(sourceFile, request.text, request.from_line);
  if (!node) return { found: false };
  const at = (pos: number): number => sourceFile.getLineAndCharacterOfPosition(pos).line + 1;
  const start = node.getStart(sourceFile);
  return {
    found: true,
    start,
    end: node.getEnd(),
    start_line: at(start),
    end_line: at(node.getEnd()),
    kind: ts.SyntaxKind[node.kind],
  };
}
