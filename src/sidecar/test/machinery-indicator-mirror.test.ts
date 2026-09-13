/**
 * carrick#371 mirror-drift guard.
 *
 * The machinery-indicator set is intentionally DUPLICATED across the capture
 * seam: `type-inferrer.ts` (ts-morph, the v1 abstain path) and
 * `capture/machinery.ts` (raw `ts`, the capture demote path) each carry their
 * own copy, because the seam forbids sharing a module across it — a capture/
 * file may import only node builtins + `typescript` + its own bundle, and the
 * rest of the sidecar may reach the bundle only via `api.js`/`index.js`
 * (enforced by capture-v2-seam.test.ts). De-duplicating into a shared module
 * would break that boundary, so the two copies are kept in lockstep by THIS
 * test instead: if they drift, one detection path silently stops abstaining and
 * the carrick#371 false verdict can reappear on whichever path lost a member.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import {
  MACHINERY_MEMBER_INDICATORS as INFERRER_SET,
  isExternalOrigin as inferrerIsExternalOrigin,
} from '../src/type-inferrer.js';
import {
  MACHINERY_MEMBER_INDICATORS as CAPTURE_SET,
  isExternalOrigin as captureIsExternalOrigin,
} from '../src/capture/machinery.js';

describe('carrick#371 machinery-indicator mirror stays in lockstep', () => {
  it('the two duplicated indicator sets are byte-for-byte equal', () => {
    const inferrer = [...INFERRER_SET].sort();
    const capture = [...CAPTURE_SET].sort();
    assert.deepStrictEqual(
      capture,
      inferrer,
      'type-inferrer.ts and capture/machinery.ts MACHINERY_MEMBER_INDICATORS ' +
        'have drifted; update both copies together (see the doc comments)'
    );
  });

  it('the set is non-empty (a truncated copy must not read as "in sync")', () => {
    assert.ok(INFERRER_SET.size >= 3, 'indicator set unexpectedly small');
  });

  it('the two origin gates answer the same for every origin shape', () => {
    // The gate is the other half of the detection: indicators alone never
    // fire. A runtime declaration Carrick materialises itself is machinery
    // origin as surely as a lib file (carrick#1017); a user's own source,
    // including one that merely sits under `.carrick/`, is not.
    const cases = [
      '/repo/node_modules/@types/node/http.d.ts',
      '/usr/lib/node/typescript/lib/lib.dom.d.ts',
      '/repo/.carrick/deno/a1b2c3d4/runtime.d.ts',
      'C:\\repo\\.carrick\\deno\\a1b2c3d4\\runtime.d.ts',
      '/repo/src/routes/orders.ts',
      '/repo/.carrick/index.json',
    ];
    for (const filePath of cases) {
      assert.strictEqual(
        captureIsExternalOrigin(filePath),
        inferrerIsExternalOrigin(filePath),
        `the two origin gates disagree on ${filePath}`
      );
    }
    assert.strictEqual(
      inferrerIsExternalOrigin('/repo/.carrick/deno/a1b2c3d4/runtime.d.ts'),
      true,
      'the materialised runtime declarations are machinery origin'
    );
    assert.strictEqual(
      inferrerIsExternalOrigin('/repo/src/routes/orders.ts'),
      false,
      'user source is never machinery origin'
    );
  });
});
