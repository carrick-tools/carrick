/**
 * Server mode: the resident sidecar for one workspace (carrick#2879).
 *
 * Pinned:
 * - a warm answer equals a cold one: `infer` through the daemon, on a second
 *   connection that reuses the held project and across two services, is
 *   byte for byte what a fresh stdio process answers, with only the timing
 *   fields (`init_time_ms`, `infer_timing`) taken out;
 * - the record (`.carrick/typed.json`) names the process, its socket and its
 *   build id, and the socket only its owner may open;
 * - a connection that does not open with `hello` naming the daemon's build is
 *   refused and closed, so a stale build never answers;
 * - one daemon per workspace: a second start exits while the first answers,
 *   and a record left by a dead process is taken over;
 * - it stops on `stop`, on SIGTERM, when idle, and when its record is gone,
 *   removing the socket and the record; `shutdown` ends a client, not it;
 * - a client killed mid-request leaves it answering;
 * - a workspace path past the socket path limit listens under the temp dir.
 *
 * Every daemon here is started on a workspace under the OS temp dir and is
 * stopped by the test that started it, or killed in `after`.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { ts } from 'ts-morph';
import { SidecarClient, SIDECAR_PATH } from './helpers.js';
import { buildIdOf } from '../src/typed-server.js';
import {
  SocketClient,
  buildId,
  daemon,
  exited,
  killDaemons,
  recordOf,
  startedDaemons,
  stopDaemon,
  untimed,
} from './typed-helpers.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
  include: ['src/**/*.ts'],
});

const SERVICE_A = `export interface Order {
  id: string;
  lines: { sku: string; qty: number }[];
}
export function makeOrder(id: string): Order {
  return { id, lines: [{ sku: 'a', qty: 1 }] };
}
export function summary(order: Order) {
  return { id: order.id, count: order.lines.length, kind: 'summary' as const };
}
export const total = (order: Order) => order.lines.reduce((sum, line) => sum + line.qty, 0);
`;

const SERVICE_B = `type Status = 'open' | 'closed';
export function status(open: boolean): { status: Status; at: number } {
  return { status: open ? 'open' : 'closed', at: Date.now() };
}
export async function load(id: string) {
  return { id, tags: [id, 'x'] as const, nested: { deep: [1, 2, 3] } };
}
`;

/** Requests for one service of workspace `ws`: function returns and an expression. */
function requestsFor(ws: string, service: string): unknown[] {
  if (service === 'a') {
    const file = path.join(ws, 'a', 'src', 'order.ts');
    return [
      { file_path: file, line_number: 5, infer_kind: 'function_return' },
      { file_path: file, line_number: 8, infer_kind: 'function_return' },
      { file_path: file, line_number: 11, infer_kind: 'expression', expression_text: 'order.lines.reduce((sum, line) => sum + line.qty, 0)' },
    ];
  }
  const file = path.join(ws, 'b', 'src', 'status.ts');
  return [
    { file_path: file, line_number: 2, infer_kind: 'function_return' },
    { file_path: file, line_number: 5, infer_kind: 'function_return' },
  ];
}

const workspaces: string[] = [];

after(() => {
  killDaemons();
  for (const ws of workspaces) fs.rmSync(ws, { recursive: true, force: true });
});

/** A workspace with two services, under the OS temp dir (or `parent`). */
function workspace(parent = os.tmpdir()): string {
  const ws = fs.mkdtempSync(path.join(parent, 'ctyped-'));
  workspaces.push(ws);
  for (const [name, file, text] of [
    ['a', 'order.ts', SERVICE_A],
    ['b', 'status.ts', SERVICE_B],
  ]) {
    fs.mkdirSync(path.join(ws, name, 'src'), { recursive: true });
    fs.writeFileSync(path.join(ws, name, 'tsconfig.json'), TSCONFIG);
    fs.writeFileSync(path.join(ws, name, 'src', file), text);
  }
  return ws;
}

async function coldInfer(ws: string, service: string): Promise<string> {
  const client = new SidecarClient();
  await client.start();
  try {
    const init = await client.send<Record<string, unknown>>({ request_id: 'i', action: 'init', repo_root: path.join(ws, service) });
    assert.strictEqual(init.status, 'ready', JSON.stringify(init));
    const answer = await client.send<Record<string, unknown>>(
      { request_id: 'q', action: 'infer', requests: requestsFor(ws, service) },
      60000
    );
    return untimed(answer);
  } finally {
    await client.stop();
  }
}

async function warmInfer(client: SocketClient, ws: string, service: string): Promise<string> {
  const init = await client.send({ request_id: 'i', action: 'init', repo_root: path.join(ws, service) });
  assert.strictEqual(init.status, 'ready', JSON.stringify(init));
  return untimed(await client.send({ request_id: 'q', action: 'infer', requests: requestsFor(ws, service) }));
}

describe('typed server (carrick#2879)', () => {
  it('answers infer through the socket exactly as a fresh stdio process does', async () => {
    const ws = workspace();
    const coldA = await coldInfer(ws, 'a');
    const coldB = await coldInfer(ws, 'b');
    assert.ok(coldA.includes('"status":"success"') && coldA.includes('inferred_types'), coldA);
    assert.ok(coldB.includes('"status":"success"') && coldB.includes('inferred_types'), coldB);

    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const first = await SocketClient.greeted(socket);
    assert.strictEqual(await warmInfer(first, ws, 'a'), coldA);
    assert.strictEqual(await warmInfer(first, ws, 'b'), coldB);
    // Again on the held projects, from a second connection.
    const second = await SocketClient.greeted(socket);
    assert.strictEqual(await warmInfer(second, ws, 'a'), coldA);
    assert.strictEqual(await warmInfer(second, ws, 'b'), coldB);
    assert.strictEqual(await warmInfer(first, ws, 'a'), coldA);

    const status = await second.send({ request_id: 'st', action: 'status' });
    const held = (status.daemon as { services: { repo_root: string; built: boolean }[] }).services;
    assert.deepStrictEqual(
      held.map((s) => [path.basename(s.repo_root), s.built]).sort(),
      [
        ['a', true],
        ['b', true],
      ]
    );
    first.conn.destroy();
    second.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('writes its record and listens on a socket only its owner may open', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const record = recordOf(ws)!;
    assert.strictEqual(record.pid, child.pid);
    assert.strictEqual(record.socket, path.join(ws, '.carrick', 'typed.sock'));
    assert.strictEqual(record.build_id, buildId());
    assert.match(record.build_id, /^[0-9a-f]{64}$/);
    assert.strictEqual(record.cli_version, '9.9.9-test');
    assert.ok(!Number.isNaN(Date.parse(record.started_at)));
    assert.strictEqual(fs.statSync(record.socket).mode & 0o777, 0o600);
    assert.strictEqual(record.workspace, fs.realpathSync(ws));
    assert.match(record.token, /^[0-9a-f]{32}$/);
    assert.strictEqual(record.node_version, process.version);
    assert.strictEqual(fs.statSync(path.join(ws, '.carrick', 'typed.json')).mode & 0o777, 0o600);

    const client = await SocketClient.greeted(record.socket);
    const status = await client.send({ request_id: 'st', action: 'status' });
    const daemonStatus = status.daemon as Record<string, unknown>;
    assert.strictEqual(daemonStatus.pid, child.pid);
    assert.strictEqual(daemonStatus.build_id, record.build_id);
    assert.deepStrictEqual(daemonStatus.services, []);
    assert.strictEqual(typeof daemonStatus.heap_used_mb, 'number');
    client.conn.destroy();
    await stopDaemon(child, record.socket);
  });

  it('refuses and closes a connection that does not open with hello naming its build', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;

    const stale = await SocketClient.connect(socket);
    const refused = await stale.send({ request_id: 'h', action: 'hello', build_id: 'f'.repeat(64), mode: 'probe' });
    assert.strictEqual(refused.status, 'error');
    assert.match(String((refused.errors as string[])[0]), /^build_mismatch/);
    // A refusal still names the daemon, so another build can tell whose it is.
    assert.deepStrictEqual([refused.pid, refused.workspace], [child.pid, fs.realpathSync(ws)]);
    await stale.closed;

    const rude = await SocketClient.connect(socket);
    const before = await rude.send({ request_id: 'i', action: 'init', repo_root: path.join(ws, 'a') });
    assert.strictEqual(before.status, 'error');
    assert.match(String((before.errors as string[])[0]), /^build_mismatch/);
    await rude.closed;

    await stopDaemon(child, socket);
  });

  it('ends a client on shutdown and keeps serving', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const client = await SocketClient.greeted(socket);
    const answer = await client.send({ request_id: 'x', action: 'shutdown' });
    assert.strictEqual(answer.status, 'success');
    await client.closed;
    assert.strictEqual(child.exitCode, null);
    await stopDaemon(child, socket);
  });

  it('keeps answering after a client is killed mid-request', async () => {
    const ws = workspace();
    const coldA = await coldInfer(ws, 'a');
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const doomed = await SocketClient.greeted(socket);
    doomed.conn.write(JSON.stringify({ request_id: 'i', action: 'init', repo_root: path.join(ws, 'b') }) + '\n');
    doomed.conn.write(JSON.stringify({ request_id: 'q', action: 'infer', requests: requestsFor(ws, 'b') }) + '\n');
    doomed.conn.destroy();

    const next = await SocketClient.greeted(socket);
    assert.strictEqual(await warmInfer(next, ws, 'a'), coldA);
    assert.strictEqual(child.exitCode, null);
    next.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('exits as already running when a daemon serves the workspace', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const second = spawn('node', [SIDECAR_PATH, '--serve', ws], { stdio: 'ignore' });
    startedDaemons.push(second);
    assert.strictEqual(await exited(second), 3);
    assert.strictEqual(recordOf(ws)!.pid, child.pid);
    await stopDaemon(child, recordOf(ws)!.socket);
  });

  it('takes over a record left by a dead process', async () => {
    const ws = workspace();
    const gone = spawn('node', ['-e', ''], { stdio: 'ignore' });
    await exited(gone);
    fs.mkdirSync(path.join(ws, '.carrick'));
    fs.writeFileSync(
      path.join(ws, '.carrick', 'typed.json'),
      JSON.stringify({ pid: gone.pid, socket: path.join(ws, '.carrick', 'typed.sock'), build_id: 'old', cli_version: null, started_at: new Date(0).toISOString() })
    );
    // A socket file left behind by the crash: bound by nobody.
    fs.writeFileSync(path.join(ws, '.carrick', 'typed.sock'), '');
    const child = await daemon(ws);
    assert.strictEqual(recordOf(ws)!.pid, child.pid);
    await stopDaemon(child, recordOf(ws)!.socket);
  });

  it('removes its socket and record on stop', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    await stopDaemon(child, socket);
    assert.strictEqual(fs.existsSync(socket), false);
    assert.strictEqual(recordOf(ws), null);
    assert.strictEqual(fs.existsSync(path.join(ws, '.carrick', 'typed.json')), false);
  });

  it('stops after its idle time with no request, removing both files', async () => {
    const ws = workspace();
    const child = await daemon(ws, { CARRICK_TYPED_IDLE_MS: '400' });
    const socket = recordOf(ws)!.socket;
    assert.strictEqual(await exited(child), 0);
    assert.strictEqual(fs.existsSync(socket), false);
    assert.strictEqual(fs.existsSync(path.join(ws, '.carrick', 'typed.json')), false);
  });

  it('stops on SIGTERM, removing both files', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    child.kill('SIGTERM');
    assert.strictEqual(await exited(child), 0);
    assert.strictEqual(fs.existsSync(socket), false);
    assert.strictEqual(fs.existsSync(path.join(ws, '.carrick', 'typed.json')), false);
  });

  it('stops when its .carrick directory is removed', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    fs.rmSync(path.join(ws, '.carrick'), { recursive: true, force: true });
    assert.strictEqual(await exited(child), 0);
  });

  it('listens under the temp dir when the workspace path is past the socket path limit', async () => {
    const top = fs.mkdtempSync(path.join(os.tmpdir(), 'ctyped-deep-'));
    workspaces.push(top);
    let parent = top;
    while (Buffer.byteLength(path.join(parent, 'ctyped-XXXXXX', '.carrick', 'typed.sock')) <= 110) {
      parent = path.join(parent, 'deep-directory-name');
    }
    fs.mkdirSync(parent, { recursive: true });
    const ws = workspace(parent);
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    assert.ok(!socket.startsWith(ws), socket);
    assert.ok(Buffer.byteLength(socket) <= 103, socket);
    // In this user's own directory, which no other user may enter (L5).
    const dir = path.dirname(socket);
    assert.strictEqual(path.basename(dir), `carrick-${process.getuid!()}`);
    assert.strictEqual(fs.statSync(dir).mode & 0o777, 0o700);
    const client = await SocketClient.greeted(socket);
    client.conn.destroy();
    await stopDaemon(child, socket);
    assert.strictEqual(fs.existsSync(socket), false);
  });

  it('runs one daemon per workspace, side by side', async () => {
    const one = workspace();
    const two = workspace();
    const first = await daemon(one);
    const second = await daemon(two);
    assert.notStrictEqual(recordOf(one)!.socket, recordOf(two)!.socket);
    await stopDaemon(first, recordOf(one)!.socket);
    await stopDaemon(second, recordOf(two)!.socket);
  });

  it('answers the server-only actions with an error over stdio', async () => {
    const client = new SidecarClient();
    await client.start();
    try {
      for (const request of [
        { request_id: '1', action: 'hello', build_id: 'x', mode: 'probe' },
        { request_id: '2', action: 'status' },
        { request_id: '3', action: 'stop' },
      ]) {
        const answer = await client.send<{ status: string; errors: string[] }>(request);
        assert.strictEqual(answer.status, 'error');
        assert.match(answer.errors[0], /server mode only/);
      }
    } finally {
      await client.stop();
    }
  });
});

describe('build id (carrick#2879)', () => {
  /** The id by the formula the Rust client hashes too: file lines, then one line per compiler package. */
  function expectedId(distSrc: string, versions: [string, string, string]): string {
    const hash = crypto.createHash('sha256');
    for (const rel of fs.readdirSync(distSrc).sort()) {
      const content = crypto.createHash('sha256').update(fs.readFileSync(path.join(distSrc, rel))).digest('hex');
      hash.update(`${rel}\0${content}\n`);
    }
    ['typescript', 'ts-morph', '@ts-morph/common'].forEach((name, i) => hash.update(`pkg:${name}\0${versions[i]}\n`));
    return hash.digest('hex');
  }

  function writePackage(dir: string, name: string, version: unknown): void {
    fs.mkdirSync(path.join(dir, 'node_modules', name), { recursive: true });
    fs.writeFileSync(path.join(dir, 'node_modules', name, 'package.json'), JSON.stringify({ name, version }));
  }

  it('hashes the installed compiler versions, found nested or hoisted, so another TypeScript is another build', () => {
    const top = fs.mkdtempSync(path.join(os.tmpdir(), 'cbid-'));
    try {
      const root = path.join(top, 'app', 'sidecar');
      const distSrc = path.join(root, 'dist', 'src');
      fs.mkdirSync(distSrc, { recursive: true });
      fs.writeFileSync(path.join(distSrc, 'index.js'), 'export {};\n');
      writePackage(root, 'typescript', '6.0.2');
      // Hoisted above the sidecar, as an npm install of a parent package lays it out.
      writePackage(top, 'ts-morph', '28.0.0');
      // Absent: ts-morph's compiler package. A version that is not a string reads as absent.
      writePackage(path.join(top, 'app'), '@ts-morph/common', 7);
      const first = buildIdOf(distSrc);
      assert.strictEqual(first, expectedId(distSrc, ['6.0.2', '28.0.0', '']));
      writePackage(root, 'typescript', '6.0.3');
      assert.notStrictEqual(buildIdOf(distSrc), first);
      assert.strictEqual(buildIdOf(distSrc), expectedId(distSrc, ['6.0.3', '28.0.0', '']));
    } finally {
      fs.rmSync(top, { recursive: true, force: true });
    }
  });

  it("names the TypeScript this build runs: the located version is the compiler's own", () => {
    const sidecarRoot = path.resolve(path.dirname(SIDECAR_PATH), '..', '..');
    const located = JSON.parse(fs.readFileSync(path.join(sidecarRoot, 'node_modules', 'typescript', 'package.json'), 'utf8')).version;
    assert.strictEqual(located, ts.version);
  });
});
