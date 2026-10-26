/**
 * carrick#2817: `list_library_surface` lists, per signature, which parts of a
 * call's return could hold the response body (`result_candidates`), and
 * leaves the listing out of the full-surface hash. The package is invented
 * (`results-fixture.ts`).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import { SidecarClient } from './helpers.js';
import { resultsService } from './results-fixture.js';

interface Candidates {
  whole_decided: boolean;
  members: string[];
  parse: string[];
}
interface Signature {
  returns: string;
  result_candidates?: Candidates;
}
interface Surface {
  package: string;
  exports: Array<{
    export: string;
    receivers: Array<{
      receiver: string;
      call?: Signature[];
      members: Array<{ name: string; signatures: Signature[] }>;
    }>;
  }>;
}
interface SurfaceResponse {
  status: string;
  surfaces: Surface[];
  surface_sha256: string;
  errors?: string[];
}

/** The fixture's full-surface hash, taken before the lister listed candidates. */
const PINNED_SHA256 = '37768e90963b2d5b224994c0fdac6bc9ebb342ff5c0a790d6de906f873610de5';

describe('list_library_surface: result candidates (carrick#2817)', () => {
  let root: string;
  let client: SidecarClient;
  let listed: SurfaceResponse;

  before(async () => {
    root = resultsService();
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
    listed = await client.send<SurfaceResponse>(
      { request_id: 'list', action: 'list_library_surface', from_dir: root, packages: ['fixture-results'] },
      60_000
    );
    assert.strictEqual(listed.status, 'success', JSON.stringify(listed.errors));
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  /** Every signature listed for `exported`'s `member` (or its call signatures when null). */
  const signatures = (exported: string, member: string | null): Signature[] => {
    const entry = listed.surfaces[0].exports.find(e => e.export === exported);
    assert.ok(entry, `export ${exported} listed`);
    const receiver = entry.receivers.find(r => r.receiver === 'export');
    assert.ok(receiver, `receiver of ${exported}`);
    if (member === null) return receiver.call ?? [];
    const found = receiver.members.find(m => m.name === member);
    assert.ok(found, `member ${member} listed`);
    return found.signatures;
  };
  const candidates = (exported: string, member: string | null, index = 0): Candidates | undefined =>
    signatures(exported, member)[index].result_candidates;

  it('1: a reply wrapper with a defaulted payload names the payload member', () => {
    assert.deepStrictEqual(candidates('api', 'get'), { whole_decided: false, members: ['data'], parse: [] });
  });

  it('2: a data/error union names both members, whichever branch carries them', () => {
    assert.deepStrictEqual(candidates('api', 'request'), { whole_decided: false, members: ['data', 'error'], parse: [] });
  });

  it('3: a return the caller supplies is decided whole', () => {
    assert.deepStrictEqual(candidates('api', 'send'), { whole_decided: true, members: [], parse: [] });
  });

  it('4: a pending object lists its parse method, not the one that returns text', () => {
    assert.deepStrictEqual(candidates('api', 'fetch'), { whole_decided: false, members: [], parse: ['json'] });
  });

  it('5: a paged wrapper lists its payload member without deciding it', () => {
    assert.deepStrictEqual(candidates('api', 'list'), { whole_decided: false, members: ['items'], parse: [] });
  });

  it('6: a hook returning a state union lists data and error, never the callable member', () => {
    assert.deepStrictEqual(candidates('useLoad', null), { whole_decided: false, members: ['data', 'error'], parse: [] });
  });

  it('7: a mutation hook lists its data members, not the ones that take an argument', () => {
    assert.deepStrictEqual(candidates('useSave', null), {
      whole_decided: false,
      members: ['data', 'error', 'variables'],
      parse: [],
    });
  });

  it('8: a plain object return lists no member', () => {
    assert.deepStrictEqual(candidates('api', 'ping'), { whole_decided: false, members: [], parse: [] });
  });

  it('9: a void return carries no candidates', () => {
    assert.strictEqual(signatures('api', 'close')[0].result_candidates, undefined);
  });

  it('lists every overload on its own', () => {
    assert.deepStrictEqual(candidates('api', 'multi', 0), { whole_decided: false, members: ['data'], parse: [] });
    assert.deepStrictEqual(candidates('api', 'multi', 1), { whole_decided: false, members: [], parse: [] });
  });

  it('leaves the returns text as it was', () => {
    assert.strictEqual(signatures('api', 'ping')[0].returns, 'Promise<{ ok: boolean; }>');
  });

  it('leaves the candidates out of the surface hash', () => {
    assert.strictEqual(listed.surface_sha256, PINNED_SHA256);
  });
});
