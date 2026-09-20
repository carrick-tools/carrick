/**
 * A request body field fed from a UI state hook holding a literal union, which
 * a bench run reported as judged `string` against a producer declaring the
 * union (carrick-tools/carrick-cloud#1119, third finding).
 *
 * Two shapes, because they have different right answers and only one of them
 * would be a defect:
 *  - the state type STATED at the hook call must survive into the body, so a
 *    producer declaring the same union agrees;
 *  - the state type left to inference is `string` by the compiler's own rule
 *    (an unconstrained type parameter widens a literal argument), and serving
 *    that is correct, not a collapse.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { SidecarClient, FIXTURES_PATH } from './helpers.js';

const FIXTURE = path.join(FIXTURES_PATH, 'src/state-hook-body.ts');
const FIXTURE_SOURCE = fs.readFileSync(FIXTURE, 'utf-8');

interface InferResponseShape {
  request_id: string;
  status: string;
  inferred_types?: Array<{
    alias: string;
    type_string: string;
    infer_kind: string;
    is_explicit: boolean;
  }>;
  errors?: string[];
}

/** Byte span of the nth occurrence of `text` (the fixture is ASCII-only). */
function spanOf(text: string, occurrence = 0): { start: number; end: number; line: number } {
  let start = -1;
  for (let i = 0; i <= occurrence; i++) {
    start = FIXTURE_SOURCE.indexOf(text, start + 1);
    assert.ok(start >= 0, `fixture must contain occurrence ${i} of: ${text}`);
  }
  const line = FIXTURE_SOURCE.slice(0, start).split('\n').length;
  return { start, end: start + text.length, line };
}

describe('a request body field fed from a state hook', () => {
  let client: SidecarClient;

  before(async () => {
    client = new SidecarClient();
    await client.start();
    await client.send({
      action: 'init',
      request_id: 'state-hook-init',
      repo_root: FIXTURES_PATH,
    });
  });

  after(async () => {
    await client.stop();
  });

  async function bodyTypeAt(text: string, occurrence: number, alias: string): Promise<string> {
    const span = spanOf(text, occurrence);
    const response = await client.send<InferResponseShape>({
      action: 'infer',
      request_id: `state-hook-${alias}`,
      requests: [
        {
          file_path: FIXTURE,
          line_number: span.line,
          span_start: span.start,
          span_end: span.end,
          infer_kind: 'request_body',
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(
      inferred,
      `expected an inferred type, got errors: ${JSON.stringify(response.errors)}`
    );
    return inferred.type_string;
  }

  it('keeps the union the hook call states', async () => {
    const typeString = await bodyTypeAt(
      "JSON.stringify({ mode, title: 'untitled' })",
      0,
      'StatedUnionBody'
    );
    assert.match(
      typeString,
      /mode: "draft" \| "published"/,
      `the stated state type must reach the body, got: ${typeString}`
    );
    // A bare literal in the same object DOES widen, which is the compiler's
    // answer for it and the reason this is not a blanket widening bug.
    assert.match(typeString, /title: string/, typeString);
  });

  it('serves string where the compiler itself widened the state type', async () => {
    const typeString = await bodyTypeAt('JSON.stringify({ mode })', 0, 'InferredLiteralBody');
    assert.match(
      typeString,
      /mode: string/,
      `an unconstrained type parameter widens the literal: ${typeString}`
    );
  });
});
