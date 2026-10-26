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
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import * as fs from 'node:fs';
import * as net from 'node:net';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient, SIDECAR_PATH } from './helpers.js';

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

const started: ChildProcess[] = [];
const workspaces: string[] = [];

after(() => {
  for (const child of started) if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
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

function recordOf(ws: string): { pid: number; socket: string; build_id: string; cli_version: string | null; started_at: string } | null {
  try {
    return JSON.parse(fs.readFileSync(path.join(ws, '.carrick', 'typed.json'), 'utf8'));
  } catch {
    return null;
  }
}

let built: string | undefined;

/** This build's id, as `--build-id` prints it. */
function buildId(): string {
  built ??= spawnSync('node', [SIDECAR_PATH, '--build-id'], { encoding: 'utf8' }).stdout.trim();
  return built;
}

async function until(what: string, ok: () => boolean | Promise<boolean>, timeoutMs = 20000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

function answers(socket: string): Promise<boolean> {
  return new Promise((resolve) => {
    const conn = net.createConnection(socket);
    conn.once('connect', () => {
      conn.destroy();
      resolve(true);
    });
    conn.once('error', () => resolve(false));
  });
}

/** Start a daemon on `ws` and wait until its socket answers. */
async function daemon(ws: string, env: NodeJS.ProcessEnv = {}): Promise<ChildProcess> {
  const child = spawn('node', [SIDECAR_PATH, '--serve', ws, '--cli-version', '9.9.9-test'], {
    stdio: ['ignore', 'ignore', 'pipe'],
    env: { ...process.env, ...env },
  });
  started.push(child);
  if (process.env.TYPED_TEST_LOG) child.stderr!.pipe(process.stderr);
  else child.stderr!.resume();
  await until('the daemon to answer', async () => {
    const record = recordOf(ws);
    return record !== null && record.pid === child.pid && (await answers(record.socket));
  });
  return child;
}

function exited(child: ChildProcess): Promise<number | null> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve(child.exitCode);
  return new Promise((resolve) => child.once('exit', (code) => resolve(code)));
}

/** A client on the daemon's socket: one terminal frame per request, progress skipped. */
class SocketClient {
  private buffer = '';
  private waiting: Array<(frame: Record<string, unknown>) => void> = [];
  readonly closed: Promise<void>;
  private constructor(readonly conn: net.Socket) {
    conn.setEncoding('utf8');
    conn.on('data', (chunk: string) => {
      this.buffer += chunk;
      const lines = this.buffer.split('\n');
      this.buffer = lines.pop() ?? '';
      for (const line of lines) {
        if (!line.trim()) continue;
        const frame = JSON.parse(line) as Record<string, unknown>;
        if (frame.status === 'progress') continue;
        this.waiting.shift()?.(frame);
      }
    });
    this.closed = new Promise((resolve) => conn.once('close', () => resolve()));
  }

  static connect(socket: string): Promise<SocketClient> {
    return new Promise((resolve, reject) => {
      const conn = net.createConnection(socket);
      conn.once('connect', () => resolve(new SocketClient(conn)));
      conn.once('error', reject);
    });
  }

  send(request: Record<string, unknown>): Promise<Record<string, unknown>> {
    return new Promise((resolve) => {
      this.waiting.push(resolve);
      this.conn.write(JSON.stringify(request) + '\n');
    });
  }

  /** Connect and say hello with this build's id. */
  static async greeted(socket: string): Promise<SocketClient> {
    const client = await SocketClient.connect(socket);
    const hello = await client.send({ request_id: 'h', action: 'hello', build_id: buildId() });
    assert.strictEqual(hello.status, 'success', JSON.stringify(hello));
    return client;
  }
}

/** The answer with only its timing fields taken out. */
function untimed(frame: Record<string, unknown>): string {
  const { infer_timing: _timing, init_time_ms: _init, ...rest } = frame;
  return JSON.stringify(rest);
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

async function stopDaemon(child: ChildProcess, socket: string): Promise<void> {
  const client = await SocketClient.greeted(socket);
  const answer = await client.send({ request_id: 's', action: 'stop' });
  assert.strictEqual(answer.status, 'success');
  assert.strictEqual(await exited(child), 0);
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
    const refused = await stale.send({ request_id: 'h', action: 'hello', build_id: 'f'.repeat(64) });
    assert.strictEqual(refused.status, 'error');
    assert.match(String((refused.errors as string[])[0]), /^build_mismatch/);
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
    started.push(second);
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
        { request_id: '1', action: 'hello', build_id: 'x' },
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
