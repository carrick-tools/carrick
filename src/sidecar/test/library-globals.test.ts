/**
 * carrick#2445: library claims on the language's own library. A global the
 * default library declares has no specifier, so its claims name it under a
 * `lib:<lib file>` key (`lib:lib.dom.d.ts`), with the compiler's version as
 * its installed version: the store keys the answer `lib:lib.dom.d.ts@<version>`.
 *
 * - The verifier reads a global as `const local = globalThis.<name>`, and its
 *   home is the default-library file set: a member the service adds in its own
 *   `declare global` block is not the library's.
 * - The lister lists the globals that one lib file declares, when the
 *   program's own `lib` setting loads it, capped as a package is.
 * - A program whose `lib` setting does not load the file answers
 *   `export_missing` for every claim on it, and lists nothing.
 *
 * The service files are hand-written; the declarations are the compiler's own
 * default library.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { ts } from 'ts-morph';
import { SidecarClient } from './helpers.js';

const tsconfig = (lib: string[]) =>
  JSON.stringify({
    compilerOptions: {
      target: 'es2020',
      lib,
      module: 'commonjs',
      moduleResolution: 'node',
      strict: true,
      skipLibCheck: true,
      // No automatic @types from the process cwd (carrick#1699).
      types: [],
    },
    include: ['src/**/*.ts'],
  });

const DOM = 'lib:lib.dom.d.ts';

type Claim = Record<string, unknown> & { kind: string };
interface Check {
  claim_id: string;
  package: string;
  export: string;
  role: string;
  receiver: string;
  claim: Claim;
}
interface VerdictResponse {
  status: string;
  verdicts?: Array<{ verdict: string; reason?: string }>;
  modules?: Array<{ package: string; resolved_file?: string; installed_version?: string; reason?: string }>;
  errors?: string[];
}
interface SurfaceResponse {
  status: string;
  surfaces?: Array<{
    package: string;
    resolved_file?: string;
    installed_version?: string;
    reason?: string;
    truncated: number;
    exports: Array<{
      export: string;
      receivers: Array<{
        receiver: string;
        call?: unknown[];
        construct?: unknown[];
        members: Array<{ name: string; own: boolean }>;
      }>;
    }>;
  }>;
  surface_sha256?: string;
  errors?: string[];
}

let sequence = 0;
const check = (pkg: string, exported: string, role: string, receiver: string, claim: Claim): Check => ({
  claim_id: `g${sequence++}`,
  package: pkg,
  export: exported,
  role,
  receiver,
  claim,
});
const op = (member: string | null, parts: Record<string, unknown>): Claim => ({ kind: 'op', op: 'request', member, ...parts });
const make = (form: 'call' | 'new', parts: Record<string, unknown> = {}): Claim => ({ kind: 'make', form, member: null, ...parts });

// `fetch(input, init)`: the URL at argument 0, the method an option of `init`,
// GET when absent. The other string keys of `init` are labelled, as the
// store's picker labels any options bag beside a positional name.
const fetchClaim = (parts: Record<string, unknown> = {}) =>
  check(DOM, 'fetch', 'http_client', 'export', op(null, {
    name: { arg: 0 },
    method_key: { arg: 1, key: 'method' },
    method_default: 'GET',
    payload: { arg: 1, key: 'body' },
    key_labels: { integrity: 'not_name', referrer: 'not_name' },
    ...parts,
  }));
// `new WebSocket(url)`: what it builds, with its URL at argument 0.
const socketClaim = (claim: Claim) => check(DOM, 'WebSocket', 'socket', 'export', claim);
// `navigator.sendBeacon(url, data)`: always a POST.
const beaconClaim = (parts: Record<string, unknown> = {}) =>
  check(DOM, 'navigator', 'http_client', 'export', op('sendBeacon', {
    name: { arg: 0 },
    payload: { arg: 1 },
    method_default: 'POST',
    ...parts,
  }));

function verdicts(response: VerdictResponse): string[] {
  assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
  return response.verdicts!.map(r => (r.verdict === 'verified' ? 'verified' : `${r.verdict} ${r.reason}`));
}

describe('library claims on the default library (carrick#2445)', () => {
  const clients: SidecarClient[] = [];
  const roots: string[] = [];
  let requestId = 0;

  async function sidecarOn(files: Record<string, string>): Promise<{ client: SidecarClient; root: string }> {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-lib-globals-'));
    roots.push(root);
    for (const [rel, text] of Object.entries(files)) {
      const file = path.join(root, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, text);
    }
    const client = new SidecarClient();
    clients.push(client);
    await client.start();
    const init = await client.send<{ status: string }>({ request_id: `init-${requestId++}`, action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready');
    return { client, root };
  }

  const verify = (client: SidecarClient, root: string, checks: Check[]) =>
    client.send<VerdictResponse>(
      { request_id: `verify-${requestId++}`, action: 'verify_library_claims', from_dir: root, checks },
      60_000
    );
  const list = async (client: SidecarClient, root: string, packages: string[], extra: Record<string, unknown> = {}) => {
    const response = await client.send<SurfaceResponse>(
      { request_id: `surface-${requestId++}`, action: 'list_library_surface', from_dir: root, packages, ...extra },
      120_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return response;
  };

  let dom: { client: SidecarClient; root: string };
  let noDom: { client: SidecarClient; root: string };

  before(async () => {
    dom = await sidecarOn({
      'tsconfig.json': tsconfig(['es2020', 'dom']),
      'package.json': JSON.stringify({ name: 'web' }),
      'src/index.ts': "export const ping = (): Promise<Response> => fetch('/ping');\n",
      // The service gives the browser a member of its own.
      'src/globals.ts': 'export {};\ndeclare global {\n  interface Navigator {\n    track(url: string): boolean;\n  }\n}\n',
    });
    noDom = await sidecarOn({
      'tsconfig.json': tsconfig(['es2020']),
      'package.json': JSON.stringify({ name: 'worker' }),
      'src/index.ts': 'export const service = 1;\n',
    });
  });

  after(async () => {
    for (const each of clients) await each.stop();
    for (const dir of roots) fs.rmSync(dir, { recursive: true, force: true });
  });

  it('verifies claims on fetch, WebSocket and sendBeacon, one invalid claim each', async () => {
    const checks = [
      fetchClaim(),
      // Invalid: `headers` takes no method word.
      fetchClaim({ method_key: { arg: 1, key: 'headers' } }),
      socketClaim(make('new', { base: { arg: 0 } })),
      // Invalid: the socket is constructed, never called.
      socketClaim(make('call', { base: { arg: 0 } })),
      beaconClaim(),
      // Invalid: the default is no method word.
      beaconClaim({ method_default: 'SEND' }),
      // The service's own member of the browser is not the library's.
      check(DOM, 'navigator', 'http_client', 'export', op('track', { name: { arg: 0 }, method_default: 'POST' })),
    ];
    const response = await verify(dom.client, dom.root, checks);
    assert.deepStrictEqual(verdicts(response), [
      'verified',
      'failed method_not_string',
      'verified',
      'failed member_not_callable',
      'verified',
      'failed method_not_word',
      'failed member_missing',
    ]);
    const module = response.modules!.find(entry => entry.package === DOM)!;
    assert.strictEqual(module.installed_version, ts.version);
    assert.strictEqual(path.basename(module.resolved_file!), 'lib.dom.d.ts');
    assert.strictEqual(module.reason, undefined);
  });

  it('answers export_missing for every claim when the lib setting does not load the file', async () => {
    const checks = [
      fetchClaim(),
      socketClaim(make('new', { base: { arg: 0 } })),
      beaconClaim(),
      // A lib file no program loads, in the DOM program too.
      check('lib:lib.webworker.d.ts', 'fetch', 'http_client', 'export', op(null, { name: { arg: 0 }, method_default: 'GET' })),
    ];
    assert.deepStrictEqual(verdicts(await verify(noDom.client, noDom.root, checks)), [
      'unchecked export_missing',
      'unchecked export_missing',
      'unchecked export_missing',
      'unchecked export_missing',
    ]);
    // A lib file the program loads, but which does not declare the global.
    const elsewhere = check('lib:lib.es5.d.ts', 'fetch', 'http_client', 'export', op(null, { name: { arg: 0 }, method_default: 'GET' }));
    assert.deepStrictEqual(verdicts(await verify(dom.client, dom.root, [checks[3], elsewhere])), [
      'unchecked export_missing',
      'unchecked export_missing',
    ]);
  });

  it('lists the globals the loaded lib file declares, with the verifier predicates', async () => {
    // `Promise` is a global another lib file declares.
    const { surfaces } = await list(dom.client, dom.root, [DOM], {
      exports: { [DOM]: ['fetch', 'navigator', 'Promise', 'WebSocket'] },
    });
    const [surface] = surfaces!;
    assert.strictEqual(surface.package, DOM);
    assert.strictEqual(surface.installed_version, ts.version);
    assert.strictEqual(path.basename(surface.resolved_file!), 'lib.dom.d.ts');
    assert.strictEqual(surface.reason, undefined);
    assert.deepStrictEqual(
      surface.exports.map(entry => entry.export),
      ['WebSocket', 'fetch', 'navigator']
    );
    const receivers = (name: string) => surface.exports.find(entry => entry.export === name)!.receivers;
    assert.ok(receivers('fetch')[0].call?.length, 'fetch is called');
    assert.deepStrictEqual(receivers('WebSocket').map(receiver => receiver.receiver), ['export', 'instance:new']);
    const members = receivers('navigator')[0].members;
    assert.strictEqual(members.find(member => member.name === 'sendBeacon')?.own, true);
    // The service's own member is not the library's surface.
    assert.ok(!members.some(member => member.name === 'track'));
  });

  it('caps the whole file as a package is, and lists nothing for a file the program does not load', async () => {
    const whole = await list(dom.client, dom.root, [DOM], { max_entries: 50 });
    const [surface] = whole.surfaces!;
    assert.ok(surface.exports.length > 0 && surface.exports.length <= 50);
    assert.ok(surface.truncated > 0);
    // Only values: an interface the file declares with no value is not listed.
    assert.ok(!surface.exports.some(entry => entry.export === 'RequestInit'));

    const [unloaded] = (await list(noDom.client, noDom.root, [DOM])).surfaces!;
    assert.deepStrictEqual(unloaded.exports, []);
  });
});
