/**
 * Verify mode, and warm == cold across an edit sequence (carrick#2886).
 *
 * Unit part: an injected warm/cold difference is reported, counted and
 * replaced by the cold answer; union member order alone is counted apart and
 * says so; a cold process that fails leaves the warm answer.
 *
 * Daemon part: one service with a contracts package reached through
 * `node_modules`, and one function per operation kind (HTTP route, GraphQL
 * resolver, socket handler, pub/sub subscriber and publisher, RPC procedure).
 * After each edit the daemon is asked `infer` (all items, and a strict subset
 * of them) and, on some steps, `resolve_definitions` and `check_v2` over a
 * pair of each protocol, on stubs captured by a process of their own (a daemon
 * runs no capture, carrick#2914):
 * - verify off: every answer equals a fresh stdio process's byte for byte
 *   (timing taken out), and `status` carries no verify counts;
 * - verify on (`CARRICK_TYPED_VERIFY=1`): every request is answered both ways
 *   and none differs.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { SocketClient, daemon, killDaemons, recordOf, stopDaemon, untimed } from './typed-helpers.js';
import { compareFrames, newVerifyCounts, verifyAnswer, verifyEnabled } from '../src/typed-verify.js';

describe('verify mode, unit (carrick#2886)', () => {
  const line = JSON.stringify({ request_id: 'q', action: 'infer' });
  const frame = (type: string) => JSON.stringify({ request_id: 'q', status: 'success', result: { type }, infer_timing: { slots_ms: 1 } });

  it('is off unless the switch is exactly 1', () => {
    assert.strictEqual(verifyEnabled({}), false);
    assert.strictEqual(verifyEnabled({ CARRICK_TYPED_VERIFY: '0' }), false);
    assert.strictEqual(verifyEnabled({ CARRICK_TYPED_VERIFY: '1' }), true);
  });

  it('reports an injected difference, counts it and returns the cold answer', async () => {
    const counts = newVerifyCounts();
    const logs: string[] = [];
    const warm = frame('{ a: string }');
    const cold = frame('{ a: number }');
    const out = await verifyAnswer(warm, null, line, async () => cold, counts, (m) => logs.push(m));
    assert.strictEqual(out.comparison, 'different');
    assert.strictEqual(out.frame, cold);
    assert.deepStrictEqual(counts, { checked: 1, mismatches: 1, order_only: 0 });
    assert.match(logs.join('\n'), /warm and cold answers differ, returned the cold answer/);
  });

  it('keeps the warm answer when they agree, ignoring timing', async () => {
    const counts = newVerifyCounts();
    const warm = frame('string');
    const cold = JSON.stringify({ request_id: 'q', status: 'success', result: { type: 'string' }, infer_timing: { slots_ms: 99 } });
    const out = await verifyAnswer(warm, null, line, async () => cold, counts, () => {});
    assert.strictEqual(out.comparison, 'equal');
    assert.strictEqual(out.frame, warm);
    assert.deepStrictEqual(counts, { checked: 1, mismatches: 0, order_only: 0 });
  });

  it('counts a difference in union member order apart, and says so', async () => {
    const counts = newVerifyCounts();
    const logs: string[] = [];
    const warm = frame('{ k: "b" | "a" | null }');
    const cold = frame('{ k: "a" | "b" | null }');
    assert.strictEqual(compareFrames(warm, cold), 'order_only');
    const out = await verifyAnswer(warm, null, line, async () => cold, counts, (m) => logs.push(m));
    assert.strictEqual(out.frame, cold);
    assert.deepStrictEqual(counts, { checked: 1, mismatches: 0, order_only: 1 });
    assert.match(logs.join('\n'), /only in union member order/);
  });

  it('leaves the warm answer when the cold process fails', async () => {
    const counts = newVerifyCounts();
    const warm = frame('string');
    const out = await verifyAnswer(warm, null, line, async () => Promise.reject(new Error('boom')), counts, () => {});
    assert.strictEqual(out.frame, warm);
    assert.strictEqual(counts.checked, 0);
  });
});

const CONTRACTS: Record<string, string> = {
  'package.json': JSON.stringify({ name: '@ws/contracts', version: '1.0.0', types: 'src/index.ts' }),
  'src/index.ts': `export * from './orders';
export * from './users';
`,
  'src/orders.ts': `export interface OrderDto {
  id: string;
  name: string;
}
export function makeOrder(): OrderDto {
  return { id: 'a', name: 'n' };
}
`,
  'src/users.ts': `export interface UserDto {
  id: string;
  email: string;
}
export function makeUser(): UserDto {
  return { id: 'u', email: 'e' };
}
`,
};

const SERVICE: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    // rootDir with a symlinked workspace package outside it raises TS6059,
    // which used to taint a first-batch reading only (carrick#2898).
    compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, rootDir: 'src', types: [] },
    include: ['src/**/*.ts'],
  }),
  'package.json': JSON.stringify({ name: 'svc', private: true }),
  'src/http.ts': `import { makeOrder } from '@ws/contracts';
export function getOrder() {
  const order = makeOrder();
  return { ...order, seen: 1 };
}
`,
  'src/graphql.ts': `import { makeOrder } from '@ws/contracts';
export function orderResolver() {
  return { ...makeOrder(), kind: 'gql' };
}
`,
  'src/socket.ts': `import { OrderDto } from '@ws/contracts';
export function onOrder(msg: OrderDto) {
  return { ...msg, ack: true };
}
`,
  'src/pubsub.ts': `import { UserDto, makeUser } from '@ws/contracts';
export function onUser(event: UserDto) {
  return { ...event, handled: true };
}
export function publishUser() {
  return { ...makeUser(), at: 1 };
}
`,
  'src/rpc.ts': `import { makeOrder } from '@ws/contracts';
export function orderProcedure() {
  return { ...makeOrder(), via: 'rpc' };
}
`,
  'src/other.ts': `export function health() {
  return { ok: true, at: 1 };
}
`,
};

/** [name, file, line, param] of each operation's function. */
const ITEMS: Array<{ name: string; file: string; line: number; param?: string }> = [
  { name: 'Http', file: 'src/http.ts', line: 2 },
  { name: 'Graphql', file: 'src/graphql.ts', line: 2 },
  { name: 'Socket', file: 'src/socket.ts', line: 2 },
  { name: 'SubscribeUser', file: 'src/pubsub.ts', line: 2 },
  { name: 'PublishUser', file: 'src/pubsub.ts', line: 5 },
  { name: 'Rpc', file: 'src/rpc.ts', line: 2 },
  { name: 'Other', file: 'src/other.ts', line: 1 },
];
/** The http route, the publisher (reads the users contract) and the unrelated leaf. */
const SUBSET = [0, 4, 6];

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
  const ws = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'cver-')));
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

function inferOf(root: string, picked: number[]): Record<string, unknown> {
  return {
    request_id: 'infer',
    action: 'infer',
    requests: picked.map((i) => ({
      file_path: path.join(root, ITEMS[i].file),
      line_number: ITEMS[i].line,
      infer_kind: 'function_return',
    })),
  };
}

function captureOf(root: string, serviceName: string, outDir: string): Record<string, unknown> {
  return {
    request_id: `capture-${serviceName}`,
    action: 'capture_v2',
    repo_root: root,
    service_name: serviceName,
    scan_root: root,
    out_dir: outDir,
    anchors: ITEMS.map((item) => ({
      kind: 'infer',
      alias: `${item.name}_Response`,
      source_file: item.file,
      anchor_origin: 'deterministic-infer',
      line_number: item.line,
    })),
  };
}

type Protocol = 'http' | 'graphql' | 'socket' | 'pubsub';
// RPC rows are judged as HTTP pairs: the probe has no protocol of its own for them.
const PAIRS: Array<[string, Protocol, string, string | undefined]> = [
  ['http', 'http', 'Http', undefined],
  ['graphql', 'graphql', 'Graphql', undefined],
  ['socket', 'socket', 'Socket', undefined],
  ['pubsub', 'pubsub', 'SubscribeUser', 'users'],
  ['rpc', 'http', 'Rpc', undefined],
];

function checkOf(dirs: { a: string; b: string }): Record<string, unknown> {
  return {
    request_id: 'check',
    action: 'check_v2',
    stubs: [
      { service_name: 'producer', stub_dir: dirs.a },
      { service_name: 'consumer', stub_dir: dirs.b },
    ],
    pairs: PAIRS.map(([key, protocol, name, topic]) => ({
      pair_key: key,
      protocol,
      type_kind: protocol === 'http' ? 'response' : 'both',
      ...(topic ? { topic } : {}),
      producer: { service_name: 'producer', alias: `${name}_Response` },
      consumer: { service_name: 'consumer', alias: `${name}_Response` },
    })),
  };
}

function definitionsOf(stubDir: string): Record<string, unknown> {
  return { request_id: 'defs', action: 'resolve_definitions', stub_dir: stubDir, aliases: ITEMS.map((i) => `${i.name}_Response`) };
}

/** Each request answered by a fresh process of its own: cold is never "a process that already answered another request". */
async function coldAnswers(root: string, requests: Array<Record<string, unknown>>): Promise<string[]> {
  const out: string[] = [];
  for (const request of requests) {
    const client = new SidecarClient();
    await client.start();
    try {
      const init = await client.send<Record<string, unknown>>({ request_id: 'i', action: 'init', repo_root: root });
      assert.strictEqual(init.status, 'ready', JSON.stringify(init));
      out.push(untimed(await client.send(request, 120000)));
    } finally {
      await client.stop();
    }
  }
  return out;
}

interface Step {
  name: string;
  change: () => void;
  /** The `infer` batches asked after the change. */
  batches: number[][];
  /**
   * 'partial': the strict-subset batch (`SUBSET`) the step asks must read some
   * of its items again and keep the others.
   */
  subset?: 'partial';
  /** Also capture (cold), resolve definitions, and judge a pair of each protocol. */
  judge: boolean;
}

async function itemCounts(client: SocketClient, root: string): Promise<{ replayed: number; recomputed: number }> {
  const status = await client.send({ request_id: 'st', action: 'status' });
  const services = (status.daemon as { services: Array<{ repo_root: string; items: { replayed: number; recomputed: number } }> }).services;
  return services.find((svc) => svc.repo_root === root)!.items;
}

/** Drive the edit sequence through a daemon; returns how many requests it sent. */
async function drive(verify: boolean): Promise<void> {
  const { ws, root, contracts } = workspace();
  const all = ITEMS.map((_, i) => i);
  const child = await daemon(ws, { CARRICK_UNWIDENED_BUDGET_MS: '100000000', ...(verify ? { CARRICK_TYPED_VERIFY: '1' } : {}) });
  const socket = recordOf(ws)!.socket;
  const client = await SocketClient.greeted(socket);
  const init = await client.send({ request_id: 'i', action: 'init', repo_root: root });
  assert.strictEqual(init.status, 'ready', JSON.stringify(init));
  const out = fs.mkdtempSync(path.join(ws, 'stubs-'));
  const dirs = { a: path.join(out, 'a'), b: path.join(out, 'b') };

  const steps: Step[] = [
    { name: 'first answer', change: () => {}, batches: [all], judge: true },
    { name: 'repeat', change: () => {}, batches: [all], judge: false },
    {
      name: 'contract shape through the barrel, then a strict subset of the batch, then all',
      change: () => edit(contracts, 'src/orders.ts', (t) => t.replace('  name: string;\n', '  name: string;\n  region?: string;\n')),
      batches: [SUBSET, all],
      subset: 'partial',
      judge: true,
    },
    {
      name: 'contract body',
      change: () => edit(contracts, 'src/orders.ts', (t) => t.replace("name: 'n'", "name: 'm'")),
      batches: [all],
      judge: false,
    },
    {
      name: 'the other contract changes shape: its readers only',
      change: () => edit(contracts, 'src/users.ts', (t) => t.replace('  email: string;\n', '  email: string;\n  name?: string;\n')),
      batches: [SUBSET, all],
      subset: 'partial',
      judge: true,
    },
    {
      name: 'leaf body',
      change: () => edit(root, 'src/other.ts', (t) => t.replace('ok: true', 'ok: false')),
      batches: [all, SUBSET],
      subset: 'partial',
      judge: false,
    },
    {
      name: 'leaf return gains a member',
      change: () => edit(root, 'src/socket.ts', (t) => t.replace('ack: true', 'ack: true, at: 1')),
      batches: [all],
      judge: true,
    },
    {
      name: 'edit undone',
      change: () => edit(root, 'src/socket.ts', (t) => t.replace('ack: true, at: 1', 'ack: true')),
      batches: [all],
      judge: false,
    },
  ];

  let sent = 0;
  const cold: Array<[string, string]> = [];
  for (const step of steps) {
    step.change();
    const requests: Array<Record<string, unknown>> = step.batches.map((b) => inferOf(root, b));
    if (step.judge) {
      // Captured as the CLI captures: by a process of its own.
      for (const answer of await coldAnswers(root, [captureOf(root, 'producer', dirs.a), captureOf(root, 'consumer', dirs.b)])) {
        assert.ok(answer.includes('"status":"success"'), `${step.name}: ${answer.slice(0, 300)}`);
      }
    }
    const warm: string[] = [];
    for (const request of requests) {
      const asked = (request.requests as unknown[] | undefined)?.length;
      const isSubset = asked === SUBSET.length && step.subset !== undefined;
      const before = isSubset ? await itemCounts(client, root) : null;
      const frame = await client.send(request);
      sent += 1;
      if (before) {
        const after = await itemCounts(client, root);
        const got = [after.recomputed - before.recomputed, after.replayed - before.replayed];
        assert.ok(got[0] > 0 && got[1] > 0 && got[0] + got[1] === SUBSET.length, `${step.name}: [read again, kept] ${got} is not a strict subset read again`);
      }
      assert.strictEqual(frame.status, 'success', `${step.name}: ${JSON.stringify(frame)}`);
      warm.push(untimed(frame));
    }
    if (step.judge) {
      for (const request of [definitionsOf(dirs.a), checkOf(dirs)]) {
        const frame = await client.send(request);
        sent += 1;
        assert.strictEqual(frame.status, 'success', `${step.name}: ${JSON.stringify(frame)}`);
        requests.push(request);
        warm.push(untimed(frame));
      }
    }
    if (!verify) {
      const want = await coldAnswers(root, requests);
      want.forEach((answer, i) => assert.strictEqual(warm[i], answer, `${step.name}: request ${i} (${requests[i].action}) warm and cold answers differ`));
      cold.push([step.name, 'ok']);
    }
  }

  const status = (await client.send({ request_id: 'st', action: 'status' })).daemon as { verify?: Record<string, number> };
  if (verify) {
    assert.deepStrictEqual(status.verify, { checked: sent, mismatches: 0, order_only: 0 });
  } else {
    assert.strictEqual(status.verify, undefined, 'status carries no verify counts while verify is off');
    assert.strictEqual(cold.length, steps.length);
  }
  client.conn.destroy();
  await stopDaemon(child, socket);
}

describe('warm == cold across an edit sequence (carrick#2886)', () => {
  it('verify off: every answer equals a fresh process, byte for byte', async () => {
    await drive(false);
  });

  it('verify on: every request is answered warm and cold, and none differs', async () => {
    await drive(true);
  });
});
