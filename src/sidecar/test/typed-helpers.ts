/**
 * Shared by the server-mode tests (carrick#2879, carrick#2880): start a
 * daemon on a workspace, talk to it over its socket, and stop it.
 */

import * as assert from 'node:assert';
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import * as fs from 'node:fs';
import * as net from 'node:net';
import * as path from 'node:path';
import { SIDECAR_PATH } from './helpers.js';

/** Every daemon a test started; `killDaemons` ends any still running. */
export const startedDaemons: ChildProcess[] = [];

export function killDaemons(): void {
  for (const child of startedDaemons) if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
}

export function recordOf(
  ws: string
): { pid: number; socket: string; build_id: string; cli_version: string | null; started_at: string; workspace: string; token: string; node_version: string } | null {
  try {
    return JSON.parse(fs.readFileSync(path.join(ws, '.carrick', 'typed.json'), 'utf8'));
  } catch {
    return null;
  }
}

let built: string | undefined;

/** This build's id, as `--build-id` prints it. */
export function buildId(): string {
  built ??= spawnSync('node', [SIDECAR_PATH, '--build-id'], { encoding: 'utf8' }).stdout.trim();
  return built;
}

export async function until(what: string, ok: () => boolean | Promise<boolean>, timeoutMs = 20000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!(await ok())) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

export function answers(socket: string): Promise<boolean> {
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
export async function daemon(ws: string, env: NodeJS.ProcessEnv = {}): Promise<ChildProcess> {
  // `--expose-gc`, as the CLI starts it: the heap budget collects before it measures.
  const child = spawn('node', ['--expose-gc', SIDECAR_PATH, '--serve', ws, '--cli-version', '9.9.9-test'], {
    stdio: ['ignore', 'ignore', 'pipe'],
    env: { ...process.env, ...env },
  });
  startedDaemons.push(child);
  if (process.env.TYPED_TEST_LOG) child.stderr!.pipe(process.stderr);
  else child.stderr!.resume();
  await until('the daemon to answer', async () => {
    const record = recordOf(ws);
    return record !== null && record.pid === child.pid && (await answers(record.socket));
  });
  return child;
}

export function exited(child: ChildProcess): Promise<number | null> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve(child.exitCode);
  return new Promise((resolve) => child.once('exit', (code) => resolve(code)));
}

/** A client on the daemon's socket: one terminal frame per request, progress skipped. */
export class SocketClient {
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

  /** Connect and say hello with this build's id: `warm` (the warm-up, which builds) unless a probe is asked for. */
  static async greeted(socket: string, mode: 'probe' | 'warm' = 'warm'): Promise<SocketClient> {
    const client = await SocketClient.connect(socket);
    const hello = await client.send({ request_id: 'h', action: 'hello', build_id: buildId(), mode });
    assert.strictEqual(hello.status, 'success', JSON.stringify(hello));
    return client;
  }
}

/** The answer with only its timing fields taken out. */
export function untimed(frame: Record<string, unknown>): string {
  const { infer_timing: _timing, init_time_ms: _init, ...rest } = frame;
  return JSON.stringify(rest);
}

export async function stopDaemon(child: ChildProcess, socket: string): Promise<void> {
  const client = await SocketClient.greeted(socket);
  const answer = await client.send({ request_id: 's', action: 'stop' });
  assert.strictEqual(answer.status, 'success');
  assert.strictEqual(await exited(child), 0);
}
