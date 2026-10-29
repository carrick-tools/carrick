/**
 * carrick#3032 (K3-A): the event names Node's runtime gives every
 * `EventEmitter` (`error`, `newListener`, `removeListener`) are a fixed
 * table, keyed to the runtime types package's own declaration of the emitter.
 * The lister marks the members that declaration owns (`runtime_events`), left
 * out of `surface_sha256`, and the verifier accepts a `reserved` claim for the
 * names from the same table, on those members only.
 *
 * Every package below is invented and its declarations are hand-written, in
 * the two layouts the runtime types package has used: `Key<K, T>` name slots
 * (`fixtures/runtime-emitter/`) and `EventNames<T, E>` (`layout-25/`).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { fullSurfaceSha256 } from '../src/library-claims.js';
import type { LibrarySurface } from '../src/types.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'runtime-emitter');
const NAMES = ['error', 'newListener', 'removeListener'];

interface Member {
  name: string;
  runtime_events?: string[];
  signatures: Array<{ params: Array<{ literals?: string[] }> }>;
}
interface Listed {
  status: string;
  surface_sha256: string;
  surfaces: Array<LibrarySurface & { exports: Array<{ export: string; receivers: Array<{ receiver: string; members: Member[] }> }> }>;
}
interface Verdicts {
  status: string;
  verdicts: Array<{ verdict: string; reason?: string }>;
}
interface Seam {
  package: string;
  export: string;
  role: string;
  claims: Array<{ member: string; of: string; name: string; picker: string }>;
}

describe('runtime emitter events (carrick#3032 K3-A)', () => {
  let client: SidecarClient;
  let roots: string[] = [];
  let requestId = 0;

  /** The fixture program at `rel`, copied to a temp directory so no outer node_modules is found. */
  function copyRoot(rel: string): string {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-runtime-emitter-'));
    const from = path.join(FIXTURE, rel);
    fs.cpSync(from, root, { recursive: true, filter: source => path.relative(from, source).split(path.sep)[0] !== 'layout-25' });
    roots.push(root);
    return root;
  }

  async function onRoot(root: string): Promise<void> {
    const init = await client.send<{ status: string }>({ request_id: `init-${requestId++}`, action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready');
  }

  async function list(root: string, packages: string[]): Promise<Listed> {
    const response = await client.send<Listed>(
      { request_id: `s-${requestId++}`, action: 'list_library_surface', from_dir: root, packages },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response));
    return response;
  }

  async function verify(root: string, checks: unknown[]): Promise<string[]> {
    const response = await client.send<Verdicts>(
      { request_id: `v-${requestId++}`, action: 'verify_library_claims', from_dir: root, checks },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response));
    return response.verdicts.map(v => `${v.verdict}${v.reason ? ` ${v.reason}` : ''}`);
  }

  /** Verdicts of `reserved` claims, each package's maker verified in the same call. */
  async function verifyReserved(root: string, checks: Array<ReturnType<typeof reserved>>): Promise<string[]> {
    const makers = [...new Set(checks.map(c => `${c.package}\u0000${c.export}`))].map(key => {
      const [pkg, exportName] = key.split('\u0000');
      return make(pkg, exportName);
    });
    const verdicts = await verify(root, [...makers, ...checks]);
    assert.deepStrictEqual(verdicts.slice(0, makers.length), makers.map(() => 'verified'));
    return verdicts.slice(makers.length);
  }

  /** The `new` of an export, which every claim on its instances needs verified first. */
  const make = (pkg: string, exportName: string) => ({
    claim_id: `make-${pkg}`,
    package: pkg,
    export: exportName,
    role: 'in_process_bus',
    receiver: 'export',
    claim: { kind: 'make', form: 'new', member: null },
  });

  const reserved = (pkg: string, exportName: string, member: string, name: string) => ({
    claim_id: `${pkg}-${member}-${name}`,
    package: pkg,
    export: exportName,
    role: 'in_process_bus',
    receiver: 'instance:new',
    claim: { kind: 'reserved', of: 'instance:new', member, name },
  });

  const membersOf = (listed: Listed, pkg: string, exportName: string): Member[] =>
    listed.surfaces
      .find(s => s.package === pkg)!
      .exports.find(e => e.export === exportName)!
      .receivers.find(r => r.receiver === 'instance:new')!.members;

  let base: string;
  let layout25: string;

  before(async () => {
    base = copyRoot('.');
    layout25 = copyRoot('layout-25');
    client = new SidecarClient();
    await client.start();
    await onRoot(base);
  });

  after(async () => {
    await client.stop();
    for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
  });

  it('marks the runtime emitter members that take an event name, and not the others', async () => {
    const members = membersOf(await list(base, ['node:events']), 'node:events', 'EventEmitter');
    const marked = (name: string) => members.find(m => m.name === name)!.runtime_events;
    assert.deepStrictEqual(marked('on'), NAMES);
    assert.deepStrictEqual(marked('emit'), NAMES);
    assert.strictEqual(marked('setMaxListeners'), undefined);
  });

  it('marks the members in the layout whose name slot is a conditional too, and never a name a signature already spells', async () => {
    await onRoot(layout25);
    const members = membersOf(await list(layout25, ['node:events']), 'node:events', 'EventEmitter');
    for (const name of ['on', 'emit']) {
      const member = members.find(m => m.name === name)!;
      const spelled = new Set(member.signatures.flatMap(s => s.params.flatMap(p => p.literals ?? [])));
      assert.ok(member.runtime_events?.includes('error'), `${name} carries no error`);
      for (const event of member.runtime_events!) assert.ok(!spelled.has(event), `${name} lists ${event} twice`);
    }
    assert.strictEqual(members.find(m => m.name === 'setMaxListeners')!.runtime_events, undefined);
    await onRoot(base);
  });

  it("marks nothing on another package's own EventEmitter class", async () => {
    const members = membersOf(await list(base, ['fixture-emitter']), 'fixture-emitter', 'EventEmitter');
    assert.deepStrictEqual(members.map(m => m.name), ['on', 'setMaxListeners']);
    assert.ok(members.every(m => m.runtime_events === undefined));
  });

  it('keeps every hash: the hash of a listing is the hash of the listing without the field', async () => {
    const listed = await list(base, ['node:events', 'fixture-emitter']);
    assert.ok(JSON.stringify(listed.surfaces).includes('runtime_events'), 'the listing carries no field');
    const stripped = JSON.parse(JSON.stringify(listed.surfaces, (key, v) => (key === 'runtime_events' ? undefined : v)));
    assert.strictEqual(listed.surface_sha256, fullSurfaceSha256(stripped));
  });

  it('verifies a reserved runtime name on the runtime emitter, and no other name or package', async () => {
    assert.deepStrictEqual(
      await verifyReserved(base, [
        reserved('node:events', 'EventEmitter', 'on', 'error'),
        reserved('node:events', 'EventEmitter', 'emit', 'newListener'),
        reserved('node:events', 'EventEmitter', 'on', 'connect'),
        reserved('node:events', 'EventEmitter', 'setMaxListeners', 'error'),
        reserved('fixture-emitter', 'EventEmitter', 'on', 'error'),
      ]),
      [
        'verified',
        'verified',
        'failed reserved_not_declared',
        'failed reserved_not_declared',
        'failed reserved_not_declared',
      ]
    );
  });

  it('verifies every claim of the seam fixture the cloud pins', async () => {
    const seam: Seam = JSON.parse(fs.readFileSync(path.join(FIXTURE, 'claims.json'), 'utf8'));
    assert.strictEqual(seam.claims.length, 6);
    const verdicts = await verifyReserved(
      base,
      seam.claims.map(c => reserved(seam.package, seam.export, c.member, c.name))
    );
    assert.deepStrictEqual(verdicts, Array(seam.claims.length).fill('verified'));
  });
});
