/**
 * The judge's message rules, exported for the held check (HM-1).
 *
 * (1) The probe text is pinned: `buildProbe(...).source` for ten fixed socket
 *     and pub/sub pairs equals the text captured before the rules were
 *     extracted.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { buildProbe } from '../src/capture/check-probe.js';
import { MESSAGE_SPECS, packageOf } from './check-probe-held-message-specs.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const GOLDEN: Record<string, string> = JSON.parse(
  fs.readFileSync(path.resolve(here, '..', '..', 'test', 'fixtures', 'check-probe-held-message.golden.json'), 'utf8')
);

describe('the judge probe text is unchanged by the message-rule extraction', () => {
  for (const { spec, tolerates } of MESSAGE_SPECS) {
    it(`${spec.pair_key}: buildProbe source equals the captured text`, () => {
      const { source } = buildProbe(spec, packageOf, undefined, () => tolerates);
      assert.equal(source, GOLDEN[spec.pair_key]);
    });
  }

  it('the golden file holds exactly the fixed specs', () => {
    assert.deepEqual(Object.keys(GOLDEN).sort(), MESSAGE_SPECS.map((m) => m.spec.pair_key).sort());
  });
});
