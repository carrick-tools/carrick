/**
 * carrick#3745: a package of many values typed by another package's generic
 * builder lists the builder's member receivers on every value, which runs the
 * lister out of heap. Past a request-wide estimate (member makers times member
 * count over such values) those values list only their `export` receiver, and
 * the surface says how many makers it left out. At or under it, the listing is
 * today's. The sidecar runs at the library builder's child heap (1630 MB).
 * Both packages are invented (see schema-values-fixture.ts).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { foreignMakerEstimate, foreignMakersLeftOut, writeSchemaValuesProject } from './schema-values-fixture.js';

interface Listed {
  status: string;
  surface_sha256: string;
  surfaces: Array<{
    package: string;
    truncated: number;
    makers_left_out?: number;
    exports: Array<{ export: string; receivers: Array<{ receiver: string; members?: Array<{ name: string }> }> }>;
  }>;
  errors?: string[];
}

const PACKAGE = 'fixture-schema-values';
const DROPPED = new Set(['handlers', 'result_candidates', 'scope_receiver', 'runtime_events', 'properties']);

describe('list_library_surface: foreign maker gate (carrick#3745)', () => {
  let client: SidecarClient;
  let big: string;
  let small: string;
  let current: string;
  let requestId = 0;

  const list = (extra: Record<string, unknown> = {}) =>
    client.send<Listed>(
      {
        request_id: `f-${requestId++}`,
        action: 'list_library_surface',
        from_dir: current,
        packages: [PACKAGE],
        handlers: true,
        types: true,
        max_entries: 1_000_000,
        ...extra,
      },
      240_000
    );
  const withDir = async <T>(dir: string, run: () => Promise<T>): Promise<T> => {
    current = dir;
    const init = await client.send<{ status: string }>({ request_id: `i-${requestId++}`, action: 'init', repo_root: dir });
    assert.strictEqual(init.status, 'ready');
    return run();
  };
  const exportOf = (listed: Listed, name: string) => listed.surfaces[0].exports.find(e => e.export === name)!;
  const receiversOf = (listed: Listed, name: string) => exportOf(listed, name).receivers.map(r => r.receiver);
  // What the hash reads: handler type ids renumber when earlier receivers drop.
  const stable = (value: unknown): unknown =>
    JSON.parse(JSON.stringify(value, (key, each) => (DROPPED.has(key) ? undefined : each)));
  const exportReceiver = (listed: Listed, name: string) =>
    stable(exportOf(listed, name).receivers.find(r => r.receiver === 'export'));

  before(async () => {
    big = fs.mkdtempSync(path.join(os.tmpdir(), 'foreign-big-'));
    small = fs.mkdtempSync(path.join(os.tmpdir(), 'foreign-small-'));
    writeSchemaValuesProject(big, 200, 40);
    writeSchemaValuesProject(small, 3, 4);
    current = big;
    client = new SidecarClient();
    await client.start({ NODE_OPTIONS: '--max-old-space-size=1630' });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(big, { recursive: true, force: true });
    fs.rmSync(small, { recursive: true, force: true });
  });

  it('lists values over the budget by their export receiver, at the builder heap', async () => {
    assert.strictEqual(foreignMakerEstimate(200, 40), 378_401);
    const listed = await withDir(big, () => list());
    assert.strictEqual(listed.status, 'success', JSON.stringify(listed.errors));
    assert.strictEqual(listed.surfaces[0].truncated, 0);
    assert.deepStrictEqual(receiversOf(listed, 'Value0'), ['export']);
    assert.deepStrictEqual(receiversOf(listed, 'hub'), ['export']);
    const members = exportOf(listed, 'Value0').receivers[0].members!.map(m => m.name);
    for (const name of ['check', 'describe', 'w0', 'w39']) assert.ok(members.includes(name), name);
    assert.strictEqual(listed.surfaces[0].makers_left_out, foreignMakersLeftOut(200, 40));
    assert.strictEqual(foreignMakersLeftOut(200, 40), 8601);
    // Values the kit does not type are untouched: the package's own client, a
    // function, and the kit's own value re-exported.
    const client = receiversOf(listed, 'client');
    assert.ok(client.includes('instance:channel'));
    assert.ok(client.includes('instance:request'));
    assert.deepStrictEqual(receiversOf(listed, 'build'), ['export', 'instance:()']);
    assert.ok(receiversOf(listed, 'defaultKit').includes('instance:w0'));
  });

  it('lists the members at the budget and leaves them out one under it', async () => {
    assert.strictEqual(foreignMakerEstimate(3, 4), 169);
    const at = await withDir(small, () => list({ foreign_maker_budget: 169 }));
    const under = await list({ foreign_maker_budget: 168 });
    assert.strictEqual(at.status, 'success', JSON.stringify(at.errors));
    assert.deepStrictEqual(receiversOf(at, 'Value0'), [
      'export',
      'instance:check',
      'instance:describe',
      'instance:w0',
      'instance:w1',
      'instance:w2',
      'instance:w3',
    ]);
    assert.deepStrictEqual(receiversOf(at, 'hub'), ['export', 'instance:join']);
    assert.strictEqual(at.surfaces[0].makers_left_out, undefined);

    assert.strictEqual(under.status, 'success', JSON.stringify(under.errors));
    assert.deepStrictEqual(receiversOf(under, 'Value0'), ['export']);
    assert.deepStrictEqual(receiversOf(under, 'hub'), ['export']);
    assert.strictEqual(under.surfaces[0].makers_left_out, foreignMakersLeftOut(3, 4));
    assert.strictEqual(foreignMakersLeftOut(3, 4), 22);
    for (const name of ['client', 'build', 'defaultKit']) {
      assert.deepStrictEqual(stable(exportOf(under, name)), stable(exportOf(at, name)), name);
    }
    for (const name of ['Value0', 'hub']) {
      assert.deepStrictEqual(exportReceiver(under, name), exportReceiver(at, name), name);
    }
  });

  it('lists at or under the budget exactly as the default does', async () => {
    const at = await withDir(small, () => list({ foreign_maker_budget: 169 }));
    const first = await list();
    const second = await list();
    assert.deepStrictEqual(first.surfaces, at.surfaces);
    assert.strictEqual(first.surface_sha256, second.surface_sha256);
  });
});
