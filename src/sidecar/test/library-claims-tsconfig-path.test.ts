/**
 * carrick#4472 T2: a verify request may name another service's tsconfig. The
 * checks are then judged in a probe-only program built from that tsconfig's
 * compiler options (its `lib`, `types`, `paths`), released after the request.
 * A request without the field is judged in the service's own program, as
 * before.
 *
 * The fixture is a repo with an app that loads no DOM lib and a kit package
 * that does, and installs its own copy of a queue package.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const here = path.dirname(fileURLToPath(import.meta.url));
// dist/test -> src/sidecar -> src -> repo root
const REPO = fs.realpathSync.native(path.join(here, '..', '..', '..', '..', 'tests', 'fixtures', 'sibling-own-install', 'repo'));
const APP = path.join(REPO, 'services', 'app');
const KIT = path.join(REPO, 'packages', 'kit');
const DOM = 'lib:lib.dom.d.ts';

interface Verdict {
  claim_id: string;
  verdict: string;
  reason?: string;
  declarations?: Array<{ decl_file: string; decl_line: number }>;
}

const checks = [
  {
    claim_id: 'channel',
    package: DOM,
    export: 'BroadcastChannel',
    role: 'in_process_bus',
    receiver: 'export',
    claim: { kind: 'make', form: 'new', member: null, name: { arg: 0 } },
  },
  {
    claim_id: 'post',
    package: DOM,
    export: 'BroadcastChannel',
    role: 'in_process_bus',
    receiver: 'instance:new',
    claim: { kind: 'op', op: 'send', member: 'postMessage', name: { bound: 'maker' }, payload: { arg: 0 } },
  },
  {
    claim_id: 'queue',
    package: 'bullmq',
    export: 'Queue',
    role: 'broker',
    receiver: 'export',
    claim: {
      kind: 'make',
      form: 'new',
      member: null,
      name: { arg: 0 },
      key_labels: { minimumVersion: 'not_name', prefix: 'not_name', recommendedMinimumVersion: 'not_name' },
    },
  },
  {
    claim_id: 'add',
    package: 'bullmq',
    export: 'Queue',
    role: 'broker',
    receiver: 'instance:new',
    claim: {
      kind: 'op',
      op: 'send',
      member: 'add',
      name: { bound: 'maker' },
      payload: { arg: 1 },
      key_labels: { jobId: 'not_name', repeatJobKey: 'not_name' },
    },
  },
];

describe('verify_library_claims: tsconfig_path (carrick#4472 T2)', () => {
  let client: SidecarClient;
  let counter = 0;

  const verify = async (extra: Record<string, unknown>) => {
    const response = await client.send<{ status: string; verdicts?: Verdict[]; errors?: string[] }>(
      {
        request_id: `verify-${counter++}`,
        action: 'verify_library_claims',
        from_dir: KIT,
        scan_root: REPO,
        checks,
        ...extra,
      },
      120_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return new Map(response.verdicts!.map((v) => [v.claim_id, v]));
  };

  before(async () => {
    client = new SidecarClient();
    await client.start();
    const ready = await client.send<{ status: string }>({
      action: 'init',
      request_id: 'init',
      repo_root: APP,
      tsconfig_path: 'tsconfig.json',
      scan_root: REPO,
    });
    assert.strictEqual(ready.status, 'ready');
  });

  after(async () => {
    await client.stop();
  });

  it('without the field, the app program (no DOM) cannot read BroadcastChannel', async () => {
    const verdicts = await verify({});
    assert.strictEqual(verdicts.get('post')!.verdict, 'unchecked');
    assert.strictEqual(verdicts.get('post')!.reason, 'export_missing');
    assert.strictEqual(verdicts.get('channel')!.reason, 'export_missing');
  });

  it("with the kit's tsconfig, the bus member is verified at its lib.dom line", async () => {
    const verdicts = await verify({ tsconfig_path: path.join(KIT, 'tsconfig.json') });
    assert.strictEqual(verdicts.get('channel')!.verdict, 'verified', verdicts.get('channel')!.reason);
    const post = verdicts.get('post')!;
    assert.strictEqual(post.verdict, 'verified', post.reason);
    assert.ok(post.declarations!.length > 0);
    for (const declared of post.declarations!) {
      assert.strictEqual(declared.decl_file, DOM);
      assert.ok(declared.decl_line > 3, `line ${declared.decl_line}`);
    }
  });

  it("with the kit's tsconfig, the kit's own queue copy is verified at its declaration", async () => {
    const verdicts = await verify({ tsconfig_path: path.join(KIT, 'tsconfig.json') });
    const add = verdicts.get('add')!;
    assert.strictEqual(add.verdict, 'verified', add.reason);
    assert.deepStrictEqual(
      add.declarations!.map((d) => `${d.decl_file}:${d.decl_line}`),
      ['packages/kit/node_modules/bullmq/index.d.ts:39']
    );
  });

  it('a relative tsconfig_path resolves from the process repo root', async () => {
    const verdicts = await verify({ tsconfig_path: '../../packages/kit/tsconfig.json' });
    assert.strictEqual(verdicts.get('post')!.verdict, 'verified', verdicts.get('post')!.reason);
  });

  it('the program is released: a later request without the field is the app program again', async () => {
    await verify({ tsconfig_path: path.join(KIT, 'tsconfig.json') });
    const verdicts = await verify({});
    assert.strictEqual(verdicts.get('post')!.reason, 'export_missing');
  });

  it('a tsconfig that does not exist is an error, not a silent app-program answer', async () => {
    const response = await client.send<{ status: string; errors?: string[] }>(
      { request_id: 'verify-missing', action: 'verify_library_claims', from_dir: KIT, checks, tsconfig_path: path.join(KIT, 'nope.json') },
      60_000
    );
    assert.strictEqual(response.status, 'error');
  });
});
