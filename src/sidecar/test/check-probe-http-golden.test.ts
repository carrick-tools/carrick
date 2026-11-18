/**
 * The judge's HTTP probe text is pinned (incompatible-rule T1, property 1):
 * `buildProbe(...).source` for seven fixed HTTP pairs equals the text captured
 * on the main branch before the relaxed re-check landed. The re-check reads
 * these probes and appends to its own copy; it must never change the judge's.
 */
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, it } from 'node:test';
import { buildProbe, unwidenedAliasOf } from '../src/capture/check-probe.js';
import { HTTP_SPECS, packageOf } from './check-probe-http-specs.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GOLDEN: Record<string, string> = JSON.parse(
  fs.readFileSync(path.resolve(here, '..', '..', 'test', 'fixtures', 'check-probe-http.golden.json'), 'utf8')
);

describe('the judge HTTP probe text is unchanged by the relaxed re-check', () => {
  for (const { spec, unwidened, tolerates } of HTTP_SPECS) {
    it(`${spec.pair_key}: buildProbe source equals the captured text`, () => {
      const { source } = buildProbe(
        spec,
        packageOf,
        unwidened ? (sent) => unwidenedAliasOf(sent.alias) : undefined,
        () => tolerates
      );
      assert.equal(source, GOLDEN[spec.pair_key]);
    });
  }

  it('the golden file holds exactly the fixed specs', () => {
    assert.deepEqual(Object.keys(GOLDEN).sort(), HTTP_SPECS.map((s) => s.spec.pair_key).sort());
  });
});
