/**
 * carrick#3855: a short locator text binds an exact match only inside the
 * statement that holds the stated line. A body read stated at a handler's
 * opening line and written below it is still bound; a same-text literal in an
 * unrelated function far below is not.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import ts from 'typescript';
import { locateNode } from '../src/capture/anchors.js';
import type { InferAnchorRequest } from '../src/capture/api.js';

const filler = Array.from({ length: 50 }, (_, i) => `// filler ${i}`).join('\n');
const SOURCE = `import { hub } from './hub';
hub.get('/status', (req, res) => {
  const value = compute();
  res.send(value);
});
${filler}
function unrelated() {
  return null;
}
hub.post('/items', async (req, res) => {
  const row = await load();
  const body = req.body;
  res.json(row);
});
`;

function request(text: string, line: number): InferAnchorRequest {
  return {
    kind: 'infer',
    alias: 'A',
    source_file: 'routes.ts',
    anchor_origin: 'response' as InferAnchorRequest['anchor_origin'],
    line_number: line,
    expression_text: text,
  };
}

const sf = ts.createSourceFile('routes.ts', SOURCE, ts.ScriptTarget.ES2022, true);
const lineOf = (needle: string) => SOURCE.slice(0, SOURCE.indexOf(needle)).split('\n').length;

describe('carrick#3855 far exact text match', () => {
  it('does not bind a same-text literal in an unrelated function below the route line', () => {
    const node = locateNode(sf, request('null', lineOf("hub.get('/status'")));
    assert.ok(
      !node || node.getText(sf) !== 'null' || node.kind !== ts.SyntaxKind.NullKeyword,
      'a null literal elsewhere in the file must not be bound'
    );
  });

  it('still binds a read written below the handler opening line', () => {
    const node = locateNode(sf, request('req.body', lineOf("hub.post('/items'")));
    assert.strictEqual(node?.getText(sf), 'req.body');
  });
});
