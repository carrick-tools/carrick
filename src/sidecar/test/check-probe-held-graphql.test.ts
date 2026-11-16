/**
 * The judge's GraphQL rules, exported for the held check (S-G1).
 *
 * (1) The probe text is pinned: `buildProbe(...).source` for twelve fixed
 *     pairs equals the text captured before the rules were extracted.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildProbe } from '../src/capture/check-probe.js';
import { FIXED_SPECS, typesOf } from './check-probe-held-graphql-specs.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GOLDEN: Record<string, string> = JSON.parse(
  fs.readFileSync(
    path.resolve(here, '..', '..', 'test', 'fixtures', 'check-probe-held-graphql.golden.json'),
    'utf8'
  )
);

describe('the judge probe text is unchanged by the rule extraction', () => {
  for (const spec of FIXED_SPECS) {
    it(`${spec.pair_key}: buildProbe source equals the captured text`, () => {
      const { source } = buildProbe(spec, (n) => `@carrick/${n}`, undefined, undefined, typesOf);
      assert.equal(source, GOLDEN[spec.pair_key]);
    });
  }

  it('the golden file holds exactly the fixed specs', () => {
    assert.deepEqual(Object.keys(GOLDEN).sort(), FIXED_SPECS.map((s) => s.pair_key).sort());
  });
});
