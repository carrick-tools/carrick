/**
 * Warm judge (carrick#2883; real vendored pnpm + tsc): a kept judge workspace
 * re-checked incrementally answers exactly what a cold check answers.
 *
 * An edit sequence across HTTP, GraphQL, socket and pub/sub pairs. At every
 * step a cold `runCheck` and a `runCheck` on the kept workspace must agree on
 * every verdict, diagnostic, error and degraded service, byte for byte. The
 * install is skipped while the manifests stay the same (stub order aside) and
 * runs again, in a fresh workspace, once they change. A file the step no
 * longer plans (a removed pair's probe) leaves the kept workspace.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { spawnSync } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { runCheck, KeptJudge } from '../src/capture/index.js';
import type { CheckPairSpec, CheckResult, CheckStubInput } from '../src/capture/api.js';
import { buildProbe } from '../src/capture/check-probe.js';
import { SocketClient, daemon, killDaemons, recordOf, stopDaemon, until } from './typed-helpers.js';

let root: string;

function writeStub(dir: string, serviceName: string, surface: string): string {
  const stubDir = path.join(dir, serviceName);
  fs.mkdirSync(path.join(stubDir, 'types'), { recursive: true });
  fs.writeFileSync(
    path.join(stubDir, 'package.json'),
    JSON.stringify(
      { name: `@carrick/${serviceName}`, version: '0.0.0-carrick', private: true, types: './types/surface.d.ts' },
      null,
      2
    ) + '\n'
  );
  fs.writeFileSync(path.join(stubDir, 'types', 'surface.d.ts'), surface);
  return stubDir;
}

function pair(
  pair_key: string,
  protocol: CheckPairSpec['protocol'],
  producerAlias: string,
  consumerAlias: string,
  over: Partial<CheckPairSpec> = {}
): CheckPairSpec {
  return {
    pair_key,
    protocol,
    type_kind: 'response',
    producer: { service_name: 'orders', alias: producerAlias },
    consumer: { service_name: 'web', alias: consumerAlias },
    ...over,
  };
}

const PAIRS: CheckPairSpec[] = [
  pair('http-order', 'http', 'Order_Sent', 'Order_Exp'),
  pair('http-list', 'http', 'OrderList_Sent', 'OrderList_Exp'),
  pair('gql-order', 'graphql', 'GqlOrder_Sent', 'GqlOrder_Exp'),
  pair('socket-status', 'socket', 'Status_Sent', 'Status_Exp'),
  pair('pubsub-created', 'pubsub', 'Created_Sub', 'Created_Pub', { topic: 'orders', type_kind: 'both' }),
];

/** The producer side; `total` is the field the contract edit changes. */
function ordersSurface(total: string, note = ''): string {
  return [
    note,
    `export type Order = { id: string; total: ${total} };`,
    'export type Order_Sent = Order;',
    'export type OrderList_Sent = { items: Order[]; next: string | null };',
    'export type GqlOrder_Sent = { data: { order: Order } };',
    "export type Status_Sent = { id: string; state: 'open' | 'closed' };",
    'export type Created_Sub = { orderId: string; total: number };',
  ].join('\n');
}

const WEB_SURFACE = [
  'export type Order_Exp = { id: string; total: number };',
  'export type OrderList_Exp = { items: { id: string; total: number }[]; next: string | null };',
  'export type GqlOrder_Exp = { data: { order: { id: string; total: number } } };',
  "export type Status_Exp = { id: string; state: 'open' | 'closed' };",
  'export type Created_Pub = { orderId: string; total: number };',
].join('\n');

/** What a cold run and a warm run must agree on (the kept dir aside). */
function verdictText(result: CheckResult): string {
  const { verdicts, errors, degraded_services, success, install_ok, isolation, ts_version } = result;
  return JSON.stringify({ verdicts, errors, degraded_services, success, install_ok, isolation, ts_version });
}

function probeFiles(dir: string): string[] {
  return fs.readdirSync(path.join(dir, 'packages', 'carrick-probes', 'probes')).sort();
}

describe('warm judge: a kept workspace answers what a cold check answers (real pnpm + tsc)', () => {
  const kept = new KeptJudge();
  let orders: string;
  let web: string;
  const stubs = (): CheckStubInput[] => [
    { service_name: 'orders', stub_dir: orders },
    { service_name: 'web', stub_dir: web },
  ];

  async function step(input: CheckStubInput[], pairs: CheckPairSpec[]): Promise<{ cold: CheckResult; warm: CheckResult }> {
    const cold = await runCheck({ stubs: input, pairs });
    const warm = await runCheck({ stubs: input, pairs, kept });
    assert.strictEqual(cold.success, true, JSON.stringify(cold.errors));
    assert.strictEqual(verdictText(warm), verdictText(cold));
    assert.strictEqual(warm.workspace_dir, '', 'a kept workspace is never handed out');
    assert.ok(!('judge' in warm), 'how the workspace was used is never in the answer');
    return { cold, warm };
  }

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-judge-warm-'));
    orders = writeStub(root, 'orders', ordersSurface('number'));
    web = writeStub(root, 'web', WEB_SURFACE);
  });

  after(() => {
    kept.discard();
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('1. first run: installs, every pair compatible', async () => {
    const { warm } = await step(stubs(), PAIRS);
    assert.strictEqual(kept.lastInstallSkipped, false);
    assert.ok(warm.verdicts.every((v) => v.bucket === 'compatible'), JSON.stringify(warm.verdicts));
  });

  it('2. a contract edit flips the verdicts that read it; the install is skipped', async () => {
    writeStub(root, 'orders', ordersSurface('string'));
    const { warm } = await step(stubs(), PAIRS);
    assert.strictEqual(kept.lastInstallSkipped, true);
    const incompatible = warm.verdicts.filter((v) => v.bucket === 'incompatible').map((v) => v.pair_key).sort();
    assert.deepStrictEqual(incompatible, ['gql-order', 'http-list', 'http-order']);
  });

  it('3. a leaf edit (no shape change), stubs sent in another order: install skipped', async () => {
    writeStub(root, 'orders', ordersSurface('string', '// a comment only'));
    const { warm } = await step([...stubs()].reverse(), PAIRS);
    assert.strictEqual(kept.lastInstallSkipped, true);
  });

  it('4. the edit reverted and a pair removed: its probe leaves the kept workspace', async () => {
    writeStub(root, 'orders', ordersSurface('number'));
    const pairs = PAIRS.filter((p) => p.pair_key !== 'socket-status');
    const { warm } = await step(stubs(), pairs);
    assert.strictEqual(kept.lastInstallSkipped, true);
    assert.ok(warm.verdicts.every((v) => v.bucket === 'compatible'), JSON.stringify(warm.verdicts));
    const removed = buildProbe(PAIRS[3], (s) => `@carrick/${s}`).fileName;
    const files = probeFiles(kept.workspaceDir!);
    assert.strictEqual(files.length, pairs.length, files.join(','));
    assert.ok(!files.includes(removed), `${removed} still in the kept workspace`);
  });

  it('5. a new stub changes the manifests: a fresh workspace, installed again', async () => {
    const before = kept.workspaceDir;
    const billing = writeStub(root, 'billing', 'export type Invoice_Sent = { id: string };');
    const pairs = [
      ...PAIRS,
      pair('http-invoice', 'http', 'Invoice_Sent', 'Order_Exp', { producer: { service_name: 'billing', alias: 'Invoice_Sent' } }),
    ];
    const { warm } = await step([...stubs(), { service_name: 'billing', stub_dir: billing }], pairs);
    assert.strictEqual(kept.lastInstallSkipped, false);
    assert.notStrictEqual(kept.workspaceDir, before);
    assert.ok(!fs.existsSync(before!), 'the replaced workspace is removed');
  });

  it('discard removes the kept workspace', () => {
    const dir = kept.workspaceDir!;
    kept.discard();
    assert.strictEqual(kept.workspaceDir, null);
    assert.ok(!fs.existsSync(dir));
  });
});

describe('warm judge: a failed compiler run keeps nothing', () => {
  const kept = new KeptJudge();
  let dir: string;

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-judge-warm-fail-'));
  });

  after(() => {
    kept.discard();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('an abnormal tsc drops the kept workspace; the next run installs again', async () => {
    const input: CheckStubInput[] = [
      { service_name: 'orders', stub_dir: writeStub(dir, 'orders', ordersSurface('number')) },
      { service_name: 'web', stub_dir: writeStub(dir, 'web', WEB_SURFACE) },
    ];
    const tscPath = path.join(dir, 'tsc-crash');
    fs.writeFileSync(tscPath, "#!/bin/sh\necho 'error TS18003: No inputs.' >&2; exit 2\n", { mode: 0o755 });
    const crashed = await runCheck({ stubs: input, pairs: PAIRS, kept, tscPath });
    assert.strictEqual(crashed.success, false);
    assert.strictEqual(kept.workspaceDir, null);
    const next = await runCheck({ stubs: input, pairs: PAIRS, kept });
    assert.strictEqual(kept.lastInstallSkipped, false);
  });
});

describe('warm judge: a daemon keeps one judge workspace across checks', () => {
  let dir: string;

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-judge-warm-daemon-'));
  });

  after(() => {
    killDaemons();
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('the second check skips the install; stopping the daemon removes the workspace', async () => {
    const ws = path.join(dir, 'ws');
    fs.mkdirSync(ws);
    const parent = path.join(dir, 'judge');
    const input: CheckStubInput[] = [
      { service_name: 'orders', stub_dir: writeStub(dir, 'orders', ordersSurface('number')) },
      { service_name: 'web', stub_dir: writeStub(dir, 'web', WEB_SURFACE) },
    ];
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const client = await SocketClient.greeted(socket);
    const check = async (id: string) =>
      (await client.send({ request_id: id, action: 'check_v2', stubs: input, pairs: PAIRS, workspace_root: parent }))
        .result as CheckResult;
    const judgeUses = async () => {
      const { checks, installs_skipped, workspace_dir, size_mb } = ((await client.send({ request_id: 'st', action: 'status' })).daemon as {
        judge: { checks: number; installs_skipped: number; workspace_dir: string | null; size_mb: number | null };
      }).judge;
      assert.ok(workspace_dir !== null && workspace_dir.startsWith(parent), String(workspace_dir));
      assert.ok(typeof size_mb === 'number' && size_mb >= 0, String(size_mb));
      return { checks, installs_skipped };
    };
    const first = await check('c1');
    assert.deepStrictEqual(await judgeUses(), { checks: 1, installs_skipped: 0 });
    // Listed where the next daemon's start reads it, should this one abort without discarding it.
    const listed = fs.readFileSync(path.join(ws, '.carrick', 'typed.judges'), 'utf8').split('\n').filter(Boolean);
    assert.deepStrictEqual(listed.map((dir) => path.dirname(dir)), [parent]);
    const second = await check('c2');
    assert.deepStrictEqual(await judgeUses(), { checks: 2, installs_skipped: 1 });
    assert.strictEqual(verdictText(second), verdictText(first));
    assert.strictEqual(fs.readdirSync(parent).length, 1, 'one kept workspace');
    client.conn.destroy();
    await stopDaemon(child, socket);
    assert.deepStrictEqual(fs.readdirSync(parent), [], 'the kept workspace leaves with the daemon');
  });

  it('a check whose client goes away kills its install, and the kept judge is free for the next check', async () => {
    const ws = path.join(dir, 'ws-abort');
    fs.mkdirSync(ws);
    const parent = path.join(dir, 'judge-abort');
    const input: CheckStubInput[] = [
      { service_name: 'orders', stub_dir: writeStub(dir, 'orders-abort', ordersSurface('number')) },
      { service_name: 'web', stub_dir: writeStub(dir, 'web-abort', WEB_SURFACE) },
    ];
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const children = (): number[] =>
      spawnSync('pgrep', ['-P', String(child.pid)], { encoding: 'utf8' }).stdout.split('\n').filter(Boolean).map(Number);
    const doomed = await SocketClient.greeted(socket);
    doomed.conn.write(JSON.stringify({ request_id: 'c1', action: 'check_v2', stubs: input, pairs: PAIRS, workspace_root: parent }) + '\n');
    await until('the install to start', () => children().length > 0, 60000);
    const install = children();
    doomed.conn.destroy();
    await until('the install to be killed', () => install.every((pid) => !alive(pid)), 20000);
    const next = await SocketClient.greeted(socket);
    type Status = { counters: { abandoned: number }; judge: { checks: number; installs_skipped: number; workspace_dir: string | null } };
    // Answered once the killed check has ended: it finished nothing, so it kept no workspace.
    const after = (await next.send({ request_id: 'st0', action: 'status' })).daemon as Status;
    assert.deepStrictEqual([after.counters.abandoned, after.judge.workspace_dir], [1, null]);
    const answer = await next.send({ request_id: 'c2', action: 'check_v2', stubs: input, pairs: PAIRS, workspace_root: parent });
    assert.strictEqual(answer.status, 'success', JSON.stringify(answer).slice(0, 500));
    const status = (await next.send({ request_id: 'st', action: 'status' })).daemon as Status;
    assert.deepStrictEqual([status.judge.checks, status.judge.installs_skipped], [2, 0], 'the next check installs afresh');
    next.conn.destroy();
    await stopDaemon(child, socket);
  });
});

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === 'EPERM';
  }
}
