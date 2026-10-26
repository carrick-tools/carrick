/**
 * carrick#2817: the `result` claim, judged over every overload on a receiver.
 * It is verified when the claimed result (the whole return, one member of it,
 * or one parse method) is a candidate of at least one signature, and refused
 * with `result_not_candidate` otherwise. A make or op claim may carry a
 * `result`; the claim it sits on is judged as if it did not.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import { SidecarClient } from './helpers.js';
import { resultsService } from './results-fixture.js';

type Claim = Record<string, unknown> & { kind: string };
interface Response {
  status: string;
  verdicts?: Array<{ verdict: string; reason?: string }>;
  errors?: string[];
}

const P = 'fixture-results';
let sequence = 0;
const check = (exported: string, claim: Claim, role = 'http_client', receiver = 'export') => ({
  claim_id: `r${sequence++}`,
  package: P,
  export: exported,
  role,
  receiver,
  claim,
});
const result = (form: 'make' | 'op', member: string | null, claimed: unknown): Claim => ({
  kind: 'result',
  form,
  member,
  result: claimed,
});
const onApi = (member: string, claimed: unknown) => check('api', result('op', member, claimed));
const onHook = (hook: string, claimed: unknown) => check(hook, result('make', null, claimed));

describe('verify_library_claims: result claims (carrick#2817)', () => {
  let root: string;
  let client: SidecarClient;
  let requestId = 0;

  const send = (checks: unknown[]) =>
    client.send<Response>({ request_id: `res-${requestId++}`, action: 'verify_library_claims', from_dir: root, checks }, 60_000);
  const verdicts = async (checks: unknown[]): Promise<string[]> => {
    const response = await send(checks);
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response.verdicts!.map(v => (v.verdict === 'verified' ? 'verified' : `${v.verdict} ${v.reason}`));
  };

  before(async () => {
    root = resultsService();
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({ request_id: 'init', action: 'init', repo_root: root });
    assert.strictEqual(ready.status, 'ready');
  });

  after(async () => {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('verifies a result that is a candidate of a signature', async () => {
    assert.deepStrictEqual(
      await verdicts([
        onApi('get', { member: ['data'] }),
        onApi('get', { whole: true }),
        onApi('request', { member: ['error'] }),
        onApi('send', { whole: true }),
        onApi('fetch', { parse: 'json' }),
        onApi('list', { member: ['items'] }),
        onHook('useLoad', { member: ['data'] }),
        onHook('useSave', { member: ['variables'] }),
      ]),
      Array(8).fill('verified')
    );
  });

  it('refuses a member that is not listed', async () => {
    assert.deepStrictEqual(
      await verdicts([onApi('get', { member: ['status'] }), onApi('ping', { member: ['ok'] }), onApi('get', { member: ['nope'] })]),
      Array(3).fill('failed result_not_candidate')
    );
  });

  it('refuses a callable member', async () => {
    assert.deepStrictEqual(
      await verdicts([onHook('useLoad', { member: ['refetch'] }), onHook('useSave', { member: ['mutateAsync'] })]),
      Array(2).fill('failed result_not_candidate')
    );
  });

  it('refuses a parse method that is not listed', async () => {
    assert.deepStrictEqual(
      await verdicts([onApi('fetch', { parse: 'text' }), onApi('get', { parse: 'json' })]),
      Array(2).fill('failed result_not_candidate')
    );
  });

  it('refuses a member where the whole return is decided', async () => {
    assert.deepStrictEqual(await verdicts([onApi('send', { member: ['data'] })]), ['failed result_not_candidate']);
  });

  it('refuses any result of a call that returns nothing', async () => {
    assert.deepStrictEqual(
      await verdicts([onApi('close', { whole: true }), onApi('close', { member: ['data'] })]),
      Array(2).fill('failed result_not_candidate')
    );
  });

  it('reads every overload: one signature that lists the result is enough', async () => {
    assert.deepStrictEqual(
      await verdicts([onApi('multi', { member: ['data'] }), onApi('multi', { whole: true }), onApi('multi', { member: ['status'] })]),
      ['verified', 'verified', 'failed result_not_candidate']
    );
  });

  it('answers the receiver failures the other claims do', async () => {
    assert.deepStrictEqual(await verdicts([onApi('absent', { whole: true })]), ['failed member_missing']);
  });

  it('judges a message role through the same rule', async () => {
    assert.deepStrictEqual(
      await verdicts([
        check('api', result('op', 'get', { member: ['data'] }), 'broker'),
        check('api', result('op', 'get', { member: ['status'] }), 'broker'),
      ]),
      ['verified', 'failed result_not_candidate']
    );
  });

  it('accepts a result on a make or op claim and judges the claim as without it', async () => {
    const bare = { kind: 'op', op: 'request', member: 'get', name: { arg: 0 }, method: 'GET' };
    const withResult = { ...bare, result: { member: ['data'] } };
    const hookBare = { kind: 'make', form: 'call', member: null, runs: { arg: 0, key: 'load' } };
    const hookWith = { ...hookBare, result: { member: ['data'] } };
    const without = await verdicts([check('api', bare), check('useLoad', hookBare)]);
    const withIt = await verdicts([check('api', withResult), check('useLoad', hookWith)]);
    assert.deepStrictEqual(withIt, without);
    // A refused result never fails its op.
    const refused = await verdicts([check('api', { ...bare, result: { member: ['status'] } })]);
    assert.deepStrictEqual(refused, [without[0]]);
  });

  it('refuses a result that names more than one of whole, member and parse', async () => {
    for (const bad of [{ whole: true, parse: 'json' }, { member: ['a', 'b'] }, { member: [] }, {}, { whole: false }]) {
      const response = await send([onApi('get', bad)]);
      assert.notStrictEqual(response.status, 'success', JSON.stringify(bad));
    }
  });
});
