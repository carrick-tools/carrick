/**
 * A held service reads again only the `infer` items an edit can reach, and
 * still answers as a cold process would (carrick#2881).
 *
 * The workspace: a contracts package, reached through `node_modules` as a
 * workspace package is, re-exported by a barrel, and a service whose files
 * each read a different part of it. After each edit, the daemon's answer to
 * one `infer` request is compared byte for byte with a fresh stdio process
 * given the same request (timing fields taken out), and `status` says how
 * many items were read again:
 * - a contract's shape changed through the barrel: the file naming it, and the
 *   file reading it through a function of that file, never the file naming
 *   another export of the same barrel;
 * - a leaf body: that file's items only;
 * - a private helper the contract's exports read: their readers;
 * - an export added: nothing reads it, but readings are read again;
 * - an export list pointed at another local: the file importing that name;
 * - a contract read by an augmentation of an installed package: every item,
 *   since a value the package returns carries it into files that import
 *   neither;
 * - a global declaration: every item;
 * - an import type (`import('m').T`) reads `T`, not every export of `m`;
 * - two items whose unwidened readings meet in a shared file: read again
 *   together, and read again when asked apart from the batch they were read in.
 * Eviction: with a 1 MB heap cap, the least recently used service is released
 * and its next request builds it again, answering as before.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { SocketClient, daemon, killDaemons, recordOf, stopDaemon, untimed } from './typed-helpers.js';

const CONTRACTS: Record<string, string> = {
  'package.json': JSON.stringify({ name: '@ws/contracts', version: '1.0.0', types: 'src/index.ts' }),
  'src/index.ts': `export * from './orders';
export * from './users';
`,
  'src/orders.ts': `function stamp(n: number) {
  return { at: n };
}
export interface OrderDto {
  id: string;
  name: string;
}
export interface InvoiceDto {
  id: string;
  total: number;
}
export function makeOrder(): OrderDto {
  return { id: 'a', name: 'n' };
}
export const stamped = () => stamp(1);
`,
  'src/users.ts': `export interface UserDto {
  id: string;
  email: string;
}
const roleA = 'admin' as const;
const roleB = 'member' as const;
export { roleA as defaultRole };
`,
};

const SERVICE: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
    include: ['src/**/*.ts'],
  }),
  'package.json': JSON.stringify({ name: 'svc', private: true }),
  'src/order-views.ts': `import { OrderDto, makeOrder } from '@ws/contracts';
export function listOrders(): OrderDto[] {
  return [makeOrder()];
}
export function viewOrder() {
  const order = makeOrder();
  return { ...order, seen: 1 };
}
export function copyOrder() {
  return { ...makeOrder() };
}
`,
  'src/report.ts': `import { copyOrder } from './order-views';
export function report() {
  return copyOrder();
}
`,
  'src/users.ts': `import { UserDto } from '@ws/contracts';
export function viewUser(user: UserDto) {
  return { ...user, seen: true };
}
`,
  'src/stamps.ts': `import { stamped } from '@ws/contracts';
export function lastStamp() {
  return stamped();
}
`,
  'src/roles.ts': `import { defaultRole } from '@ws/contracts';
export function role() {
  return { role: defaultRole };
}
`,
  'src/typed.ts': `export function viaImportType(user: import('@ws/contracts').UserDto) {
  return { ...user };
}
`,
  // An installed package's interface gains a member read from the
  // contracts: a value the package returns carries it, though the file
  // reading that value imports neither the interface nor the augmentation.
  'node_modules/@ws/lib/package.json': JSON.stringify({ name: '@ws/lib', version: '1.0.0', types: 'index.d.ts' }),
  'node_modules/@ws/lib/index.d.ts': `export interface Settings {
  mode: string;
}
export declare function load(): Settings;
`,
  'src/augment.ts': `import { UserDto } from '@ws/contracts';
declare module '@ws/lib' {
  interface Settings {
    owner?: { [K in keyof UserDto]: UserDto[K] };
  }
}
`,
  'src/use-settings.ts': `import { load } from '@ws/lib';
export function settings() {
  return { ...load() };
}
`,
  'src/env.d.ts': `interface Env {
  region: string;
}
`,
  // Marking `reset`'s literal makes the last line an error outside any
  // function: every reading with a function in this file is dropped, so
  // `h1`'s reading depends on whether `h2` shares its batch.
  'src/shared.ts': `export function pick(flag: boolean) {
  const picked = { mode: flag ? 'a' : 'b' };
  return picked;
}
export function reset() {
  const state = { mode: 'x' };
  return state;
}
export const current = reset();
current.mode = 'y';
`,
  'src/r1.ts': `import { pick } from './shared';
export function h1() {
  return pick(true);
}
`,
  'src/r2.ts': `import { reset } from './shared';
export function h2() {
  return reset();
}
`,
};

/** Each item: file and line of a function whose return is inferred. */
const ITEMS: Array<[string, number]> = [
  ['src/order-views.ts', 2], // 0: declared return, no unwidened reading
  ['src/order-views.ts', 5], // 1
  ['src/report.ts', 2], // 2: reads the contract through order-views' function; no reading, so coupled to nothing
  ['src/users.ts', 2], // 3: names another export of the barrel
  ['src/stamps.ts', 2], // 4: reads the contract file's private helper
  ['src/r1.ts', 2], // 5
  ['src/r2.ts', 2], // 6
  ['src/roles.ts', 2], // 7: an export list names its local
  ['src/typed.ts', 1], // 8: reads one export through an import type
  ['src/use-settings.ts', 2], // 9: reads an augmentation through a value an installed package returns
];
const WITHOUT_R2 = ITEMS.filter(([file]) => file !== 'src/r2.ts');

const workspaces: string[] = [];

after(() => {
  killDaemons();
  for (const ws of workspaces) fs.rmSync(ws, { recursive: true, force: true });
});

function write(root: string, file: string, text: string): void {
  fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
  fs.writeFileSync(path.join(root, file), text);
}

function workspace(): { ws: string; root: string; contracts: string } {
  const ws = fs.mkdtempSync(path.join(os.tmpdir(), 'caff-'));
  workspaces.push(ws);
  const contracts = path.join(ws, 'contracts');
  const root = path.join(ws, 'svc');
  for (const [file, text] of Object.entries(CONTRACTS)) write(contracts, file, text);
  for (const [file, text] of Object.entries(SERVICE)) write(root, file, text);
  fs.mkdirSync(path.join(root, 'node_modules', '@ws'), { recursive: true });
  fs.symlinkSync(contracts, path.join(root, 'node_modules', '@ws', 'contracts'), 'dir');
  return { ws, root, contracts };
}

function edit(root: string, file: string, change: (text: string) => string): void {
  const abs = path.join(root, file);
  const before = fs.readFileSync(abs, 'utf8');
  const after = change(before);
  assert.notStrictEqual(after, before, `the edit of ${file} changes nothing`);
  fs.writeFileSync(abs, after);
}

function inferOf(root: string, items: Array<[string, number]> = ITEMS): Record<string, unknown> {
  return {
    request_id: 'q',
    action: 'infer',
    requests: items.map(([file, line]) => ({ file_path: path.join(root, file), line_number: line, infer_kind: 'function_return' })),
  };
}

async function cold(root: string, request: Record<string, unknown>): Promise<string> {
  const client = new SidecarClient();
  await client.start();
  try {
    const init = await client.send<Record<string, unknown>>({ request_id: 'i', action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready', JSON.stringify(init));
    return untimed(await client.send(request, 60000));
  } finally {
    await client.stop();
  }
}

interface ItemCounts {
  held: number;
  replayed: number;
  recomputed: number;
}

async function serviceStatus(client: SocketClient, root: string): Promise<{ items: ItemCounts; evictions: number }> {
  const status = await client.send({ request_id: 'st', action: 'status' });
  const services = (status.daemon as { services: Array<{ repo_root: string; items: ItemCounts; evictions: number }> }).services;
  return services.find((s) => s.repo_root === root)!;
}

describe('affected set and kept infer items (carrick#2881)', () => {
  it('reads again only what each edit reaches, and answers as a cold process', async () => {
    const { ws, root, contracts } = workspace();
    const child = await daemon(ws, { CARRICK_UNWIDENED_BUDGET_MS: '100000000' });
    const socket = recordOf(ws)!.socket;
    const client = await SocketClient.greeted(socket);
    const init = await client.send({ request_id: 'i', action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready', JSON.stringify(init));

    // [name, edit, the request, the indices in ITEMS of the items it must read again]
    const all = ITEMS.map((_, index) => index);
    const steps: Array<[string, () => void, Array<[string, number]>, number[]]> = [
      ['first answer', () => {}, ITEMS, all],
      ['repeat', () => {}, ITEMS, []],
      [
        'contract shape through the barrel',
        () => edit(contracts, 'src/orders.ts', (t) => t.replace('  name: string;\n', '  name: string;\n  region?: string;\n')),
        ITEMS,
        [0, 1, 2],
      ],
      ['contract body', () => edit(contracts, 'src/orders.ts', (t) => t.replace("name: 'n'", "name: 'm'")), ITEMS, [0, 1, 2]],
      ['leaf body', () => edit(root, 'src/users.ts', (t) => t.replace('seen: true', 'seen: false')), ITEMS, [3]],
      [
        'private helper',
        () => edit(contracts, 'src/orders.ts', (t) => t.replace('return { at: n };', 'return { at: n, by: 1 };')),
        ITEMS,
        [4],
      ],
      [
        'unwidened readings that share a file: read again together',
        () => edit(root, 'src/r2.ts', (t) => t.replace('return reset();', 'const out = reset();\n  return out;')),
        ITEMS,
        [5, 6],
      ],
      ['one of them asked without the other: read again', () => {}, WITHOUT_R2, [5]],
      ['and with it again', () => {}, ITEMS, [5, 6]],
      [
        'export added: every item with a reading',
        () => edit(contracts, 'src/users.ts', (t) => `${t}export const roles = ['a'];\n`),
        ITEMS,
        [1, 3, 5, 6],
      ],
      [
        'export pointed at another local: its importer, and every item with a reading',
        () => edit(contracts, 'src/users.ts', (t) => t.replace('export { roleA as defaultRole }', 'export { roleB as defaultRole }')),
        ITEMS,
        [1, 3, 5, 6, 7],
      ],
      [
        'a contract an augmentation reads: every item',
        () => edit(contracts, 'src/users.ts', (t) => t.replace('  email: string;\n', '  email: string;\n  name?: string;\n')),
        ITEMS,
        all,
      ],
      ['global declaration', () => edit(root, 'src/env.d.ts', (t) => t.replace('region: string;', 'region: string;\n  zone: string;')), ITEMS, all],
    ];
    for (const [name, change, items, readAgain] of steps) {
      change();
      const request = inferOf(root, items);
      const before = await serviceStatus(client, root);
      const got = untimed(await client.send(request));
      const after = await serviceStatus(client, root);
      const want = await cold(root, request);
      assert.strictEqual(got, want, `${name}: warm and cold answers differ`);
      assert.ok(got.includes('"status":"success"'), `${name}: ${got}`);
      const expected = readAgain.length;
      assert.deepStrictEqual(
        [after.items.recomputed - before.items.recomputed, after.items.replayed - before.items.replayed],
        [expected, items.length - expected],
        `${name}: [read again, kept]`
      );
    }
    client.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('reaches a JavaScript file that names a type only in JSDoc, and answers as a cold process', async () => {
    const ws = fs.mkdtempSync(path.join(os.tmpdir(), 'cjsd-'));
    workspaces.push(ws);
    const root = path.join(ws, 'svc');
    const files: Record<string, string> = {
      'tsconfig.json': JSON.stringify({
        compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, allowJs: true, checkJs: true, noEmit: true, types: [] },
        include: ['src/**/*'],
      }),
      'package.json': JSON.stringify({ name: 'svc', private: true }),
      'src/models.ts': `export interface User {
  id: string;
}
export interface Order {
  id: string;
}
export interface Invoice {
  id: string;
}
export interface Account {
  id: string;
}
`,
      // A typedef on one statement, read by another statement's @param, in a module.
      'src/handlers.js': `/** @typedef {import('./models').User} User */
export const kind = 'user';
/** @param {User} u */
export function handle(u) {
  return { ...u };
}
`,
      // A typedef after the last statement.
      'src/accounts.js': `/** @param {Account} a */
export function show(a) {
  return { ...a };
}
/** @typedef {import('./models').Account} Account */
`,
      // An @import tag, read by @param.
      'src/orders.js': `/** @import { Order } from './models' */
/** @param {Order} o */
function copy(o) {
  return { ...o };
}
module.exports = { copy };
`,
      // An @type naming the type through an import type.
      'src/invoices.js': `/** @type {import('./models').Invoice} */
const base = { id: 'a' };
function dup() {
  return { ...base };
}
module.exports = { dup };
`,
      // `infer` answers TypeScript files: each reads one JavaScript file's function.
      'src/view-handlers.ts': `import { handle } from './handlers';
export function viewHandle(u: Parameters<typeof handle>[0]) {
  return handle(u);
}
`,
      'src/view-orders.ts': `import { copy } from './orders';
export function viewCopy(o: Parameters<typeof copy>[0]) {
  return copy(o);
}
`,
      'src/view-accounts.ts': `import { show } from './accounts';
export function viewShow(a: Parameters<typeof show>[0]) {
  return show(a);
}
`,
      'src/view-invoices.ts': `import { dup } from './invoices';
export function viewDup() {
  return dup();
}
`,
    };
    for (const [file, text] of Object.entries(files)) write(root, file, text);
    const items: Array<[string, number]> = [
      ['src/view-handlers.ts', 2], // 0
      ['src/view-orders.ts', 2], // 1
      ['src/view-invoices.ts', 2], // 2
      ['src/view-accounts.ts', 2], // 3
    ];
    const child = await daemon(ws, { CARRICK_UNWIDENED_BUDGET_MS: '100000000' });
    const socket = recordOf(ws)!.socket;
    const client = await SocketClient.greeted(socket);
    assert.strictEqual((await client.send({ request_id: 'i', action: 'init', repo_root: root })).status, 'ready');
    const addField = (name: string) => () =>
      edit(root, 'src/models.ts', (t) => t.replace(`export interface ${name} {\n  id: string;\n`, `export interface ${name} {\n  id: string;\n  extra${name}: number;\n`));
    const steps: Array<[string, () => void, number[]]> = [
      ['first answer', () => {}, [0, 1, 2, 3]],
      ['@typedef import type', addField('User'), [0]],
      ['@import tag', addField('Order'), [1]],
      ['@type import type', addField('Invoice'), [2]],
      ['@typedef after the last statement', addField('Account'), [3]],
      ['the typedef itself edited', () => edit(root, 'src/handlers.js', (t) => t.replace("import('./models').User", "import('./models').Order")), [0]],
    ];
    for (const [name, change, readAgain] of steps) {
      change();
      const request = inferOf(root, items);
      const before = await serviceStatus(client, root);
      const got = untimed(await client.send(request));
      const after = await serviceStatus(client, root);
      assert.strictEqual(got, await cold(root, request), `${name}: warm and cold answers differ`);
      assert.ok(got.includes('"status":"success"'), `${name}: ${got}`);
      assert.deepStrictEqual(
        [after.items.recomputed - before.items.recomputed, after.items.replayed - before.items.replayed],
        [readAgain.length, items.length - readAgain.length],
        `${name}: [read again, kept]`
      );
    }
    client.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('releases the least recently used service over its heap cap, and builds it again on its next request', async () => {
    const one = workspace();
    const two = workspace();
    // One daemon serves both: it is the first workspace's, and the second service is init'd on it.
    const child = await daemon(one.ws, { CARRICK_TYPED_HEAP_CAP_MB: '1', CARRICK_UNWIDENED_BUDGET_MS: '100000000' });
    const socket = recordOf(one.ws)!.socket;
    const a = await SocketClient.greeted(socket);
    const b = await SocketClient.greeted(socket);
    // Every request ends over a 1 MB heap, so each releases every other held service.
    assert.strictEqual((await a.send({ request_id: 'i', action: 'init', repo_root: one.root })).status, 'ready');
    const first = untimed(await a.send(inferOf(one.root)));
    assert.strictEqual((await serviceStatus(a, one.root)).items.held, ITEMS.length);
    assert.strictEqual((await b.send({ request_id: 'i', action: 'init', repo_root: two.root })).status, 'ready');
    // Read on b's connection: the request that reads it releases nothing more.
    const released = await serviceStatus(b, one.root);
    assert.deepStrictEqual([released.evictions, released.items.held], [1, 0]);
    const again = untimed(await a.send(inferOf(one.root)));
    assert.strictEqual(again, first, 'the rebuilt service answers as before');
    assert.strictEqual(again, await cold(one.root, inferOf(one.root)));
    const rebuilt = await serviceStatus(b, one.root);
    assert.deepStrictEqual([rebuilt.items.held, rebuilt.items.recomputed], [ITEMS.length, 2 * ITEMS.length]);
    assert.strictEqual((await serviceStatus(a, two.root)).evictions, 1);
    a.conn.destroy();
    b.conn.destroy();
    await stopDaemon(child, socket);
  });
});
