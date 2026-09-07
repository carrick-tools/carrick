/**
 * The README's protocol section is checked against the validators, not read
 * (carrick#755).
 *
 * It drifted far enough that a request written from it was rejected — the
 * `infer` example used field names the schema has never had, and six actions
 * were missing entirely. Prose cannot be trusted to keep up with a schema, so
 * this test holds it: every JSON request example must parse, and every action
 * in the discriminated union must appear in the README.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  parseRequest,
  validateInferRequestItem,
  SidecarRequestSchema,
} from '../src/validators.js';
import type { InferRequestItem } from '../src/types.js';

const readmePath = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../README.md',
);
const readme = fs.readFileSync(readmePath, 'utf8');

/** Every fenced ```json block in the README, in document order. */
const jsonBlocks = [...readme.matchAll(/```json\n([\s\S]*?)```/g)].map(
  (match, index) => ({ index, text: match[1] }),
);

describe('README protocol examples (#755)', () => {
  it('has JSON examples at all', () => {
    assert.ok(jsonBlocks.length > 10, `only ${jsonBlocks.length} json blocks`);
  });

  it('parses every fenced json block', () => {
    for (const block of jsonBlocks) {
      assert.doesNotThrow(
        () => JSON.parse(block.text),
        `block ${block.index} is not JSON:\n${block.text}`,
      );
    }
  });

  it('validates every request example against the schema', () => {
    let requests = 0;
    for (const block of jsonBlocks) {
      const json = JSON.parse(block.text) as Record<string, unknown>;
      // A block is a request iff it carries an action; the rest are responses
      // and progress frames.
      if (!('action' in json)) continue;
      requests += 1;
      const result = parseRequest(json);
      assert.ok(
        result.success,
        `block ${block.index} (${String(json.action)}) is rejected by the schema: ${
          result.success ? '' : result.error
        }`,
      );
      if (json.action === 'infer') {
        for (const item of json.requests as InferRequestItem[]) {
          assert.strictEqual(
            validateInferRequestItem(item),
            null,
            `block ${block.index}: infer item is rejected per-item`,
          );
        }
      }
    }
    assert.ok(requests > 0, 'no request examples found in the README');
  });

  it('documents every action the sidecar accepts', () => {
    const actions = SidecarRequestSchema.options.map(
      (option) => option.shape.action.value as string,
    );
    const documented = new Set(
      jsonBlocks
        .map((block) => JSON.parse(block.text) as Record<string, unknown>)
        .filter((json) => 'action' in json)
        .map((json) => String(json.action)),
    );
    const missing = actions.filter((action) => !documented.has(action));
    assert.deepStrictEqual(
      missing,
      [],
      `actions with no README request example: ${missing.join(', ')}`,
    );
  });
});
