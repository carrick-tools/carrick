/**
 * The daemon's lifecycle policy, sidecar side (L2-L6 of the warm type process
 * plan):
 * - a line queued by a client that went away is skipped, does no work, and
 *   is counted;
 * - a probe (a check) never builds: its `init` for a service with no built
 *   program, or one marked cold_only, is refused; a warm `init` builds;
 * - every daemon-only failure closes the connection with no frame (the client
 *   answers through a process of its own): a probe request after an edit the
 *   held program cannot take (it is released), files that keep changing,
 *   `add_program_files`;
 * - a record naming another workspace's daemon is stale and taken over;
 * - a `stop` carrying the record's token stops the daemon from any build;
 * - under its heap cap, a warm build never releases a service a probe used:
 *   it releases its own service and marks it cold_only:no_room; without the
 *   probe, the least recently used service is released instead;
 * - `status` reports states, footprints, counters and the error ring.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import { spawn, type ChildProcess } from 'node:child_process';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SocketClient, daemon, exited, killDaemons, recordOf, stopDaemon, until } from './typed-helpers.js';
import { JUDGE_OWNER_FILE, removeOrphanedJudgeWorkspaces } from '../src/capture/kept-judge.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
  include: ['src/**/*.ts'],
});

const workspaces: string[] = [];
const writers: ChildProcess[] = [];

after(() => {
  for (const writer of writers) writer.kill('SIGKILL');
  killDaemons();
  for (const ws of workspaces) fs.rmSync(ws, { recursive: true, force: true });
});

/** A workspace with services `a`, `b` and `c`, each one file and its tsconfig. */
function workspace(): string {
  const ws = fs.mkdtempSync(path.join(os.tmpdir(), 'clife-'));
  workspaces.push(ws);
  for (const name of ['a', 'b', 'c']) {
    fs.mkdirSync(path.join(ws, name, 'src'), { recursive: true });
    fs.writeFileSync(path.join(ws, name, 'tsconfig.json'), TSCONFIG);
    fs.writeFileSync(
      path.join(ws, name, 'src', 'main.ts'),
      `export function make() {\n  return { id: '${name}', n: 1 };\n}\nexport interface Made {\n  id: string;\n}\n`
    );
    fs.writeFileSync(path.join(ws, name, 'src', 'util.ts'), `export const flag = true;\n`);
  }
  return ws;
}

function initOf(ws: string, name: string): Record<string, unknown> {
  return { request_id: `i-${name}`, action: 'init', repo_root: path.join(ws, name) };
}

function inferOf(ws: string, name: string): Record<string, unknown> {
  return {
    request_id: `q-${name}`,
    action: 'infer',
    requests: [{ file_path: path.join(ws, name, 'src', 'main.ts'), line_number: 1, infer_kind: 'function_return' }],
  };
}

function captureOf(ws: string, name: string, out: string): Record<string, unknown> {
  return {
    request_id: `c-${name}`,
    action: 'capture_v2',
    repo_root: path.join(ws, name),
    service_name: name,
    out_dir: out,
    anchors: [{ kind: 'symbol', alias: 'Alias_made', symbol_name: 'Made', source_file: 'src/main.ts', anchor_origin: 'llm-symbol' }],
  };
}

/** The answer, or `closed` when the daemon closes the connection instead. */
async function answerOrClose(client: SocketClient, request: Record<string, unknown>): Promise<Record<string, unknown> | 'closed'> {
  return Promise.race([client.send(request), client.closed.then(() => 'closed' as const)]);
}

interface Status {
  services: Array<{ repo_root: string; state: string; footprint_mb: number | null; last_used_s: number | null; built: boolean }>;
  counters: Record<string, number>;
  errors: Array<{ kind: string; service: string | null; detail: Record<string, unknown> }>;
  heap_used_mb: number;
  heap_cap_mb: number;
  heap_limit_mb: number;
  warming: boolean;
  queued: number;
  collections: number;
}

async function statusOf(socket: string): Promise<Status> {
  const client = await SocketClient.greeted(socket, 'probe');
  try {
    return (await client.send({ request_id: 'st', action: 'status' })).daemon as Status;
  } finally {
    client.conn.destroy();
  }
}

function serviceIn(status: Status, ws: string, name: string): Status['services'][number] | undefined {
  return status.services.find((s) => s.repo_root === path.join(ws, name) || s.repo_root === path.join(fs.realpathSync(ws), name));
}

describe('daemon lifecycle (warm type process L2-L6)', () => {
  it('skips the queued lines of a client that went away, doing no work for them', async () => {
    const ws = workspace();
    // Verify mode: each answer waits on a cold process, so the queue yields to I/O while it runs.
    const child = await daemon(ws, { CARRICK_TYPED_VERIFY: '1' });
    const socket = recordOf(ws)!.socket;
    const busy = await SocketClient.greeted(socket);
    assert.strictEqual((await busy.send(initOf(ws, 'a'))).status, 'ready');
    const answered = busy.send(inferOf(ws, 'a'));
    // Queued behind the request above, then gone.
    const gone = await SocketClient.connect(socket);
    gone.conn.write(
      [
        JSON.stringify({ request_id: 'h', action: 'hello', build_id: recordOf(ws)!.build_id, mode: 'warm' }),
        JSON.stringify(initOf(ws, 'b')),
        JSON.stringify(inferOf(ws, 'b')),
      ].join('\n') + '\n'
    );
    await new Promise((resolve) => setTimeout(resolve, 100));
    gone.conn.destroy();
    assert.strictEqual((await answered).status, 'success');
    busy.conn.destroy();
    const status = await statusOf(socket);
    assert.strictEqual(status.counters.skipped, 3, JSON.stringify(status.counters));
    assert.strictEqual(serviceIn(status, ws, 'b'), undefined, 'the gone client built nothing');
    await stopDaemon(child, socket);
  });

  it('refuses a probe init until a warm request has built the service, and releases it when a probe meets an edit it cannot take', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;

    const probe = await SocketClient.greeted(socket, 'probe');
    const refused = await probe.send(initOf(ws, 'a'));
    assert.deepStrictEqual([refused.status, refused.errors], ['error', ['not_held: not_built']]);

    const warm = await SocketClient.greeted(socket, 'warm');
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    assert.strictEqual((await warm.send(inferOf(ws, 'a'))).status, 'success');
    let status = await statusOf(socket);
    const built = serviceIn(status, ws, 'a')!;
    assert.strictEqual(built.state, 'built');
    assert.ok(typeof built.footprint_mb === 'number' && built.footprint_mb > 0, JSON.stringify(built));
    assert.strictEqual(typeof built.last_used_s, 'number');

    assert.strictEqual((await probe.send(initOf(ws, 'a'))).status, 'ready');
    assert.strictEqual((await probe.send(inferOf(ws, 'a'))).status, 'success');

    // An import added: the held program is built again from its init, which a probe never does.
    const main = path.join(ws, 'a', 'src', 'main.ts');
    fs.writeFileSync(main, `import { flag } from './util';\n${fs.readFileSync(main, 'utf8').replace('n: 1', 'n: flag')}`);
    assert.strictEqual(await answerOrClose(probe, inferOf(ws, 'a')), 'closed');

    status = await statusOf(socket);
    assert.strictEqual(serviceIn(status, ws, 'a')!.state, 'released');
    assert.deepStrictEqual(
      status.errors.map((e) => [e.kind, e.service]),
      [
        ['init_refused', 'a'],
        ['released_on_drop', 'a'],
      ]
    );
    assert.strictEqual(status.counters.init_refused, 1);
    assert.strictEqual(status.counters.closed_for_cold, 1);
    const again = await SocketClient.greeted(socket, 'probe');
    assert.deepStrictEqual((await again.send(initOf(ws, 'a'))).errors, ['not_held: not_built']);
    again.conn.destroy();
    warm.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('closes, with no frame, a request whose files keep changing, and add_program_files', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const warm = await SocketClient.greeted(socket);
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    assert.strictEqual((await warm.send(inferOf(ws, 'a'))).status, 'success');
    const util = path.join(ws, 'a', 'src', 'util.ts');
    // Another process rewrites a program file every millisecond: every answer reads a file that changed under it.
    const writer = spawn(
      'node',
      ['-e', `const fs = require('fs'); let n = 0; setInterval(() => fs.writeFileSync(${JSON.stringify(util)}, 'export const flag = ' + (n++) + ';\\n'), 1);`],
      { stdio: 'ignore' }
    );
    writers.push(writer);
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert.strictEqual(await answerOrClose(warm, inferOf(ws, 'a')), 'closed');
    writer.kill('SIGKILL');
    await exited(writer);

    const other = await SocketClient.greeted(socket);
    assert.strictEqual((await other.send(initOf(ws, 'b'))).status, 'ready');
    const added = await answerOrClose(other, { request_id: 'add', action: 'add_program_files', files: [path.join(ws, 'b', 'src', 'util.ts')] });
    assert.strictEqual(added, 'closed');

    const status = await statusOf(socket);
    assert.ok(status.errors.some((e) => e.kind === 'unstable' && e.service === 'a'), JSON.stringify(status.errors));
    assert.strictEqual(status.counters.closed_for_cold, 2);
    await stopDaemon(child, socket);
  });

  it("takes over a record that names another workspace's daemon", async () => {
    const first = workspace();
    const original = await daemon(first);
    const copy = workspace();
    // The workspace copied with its record: the record names the first daemon, alive.
    fs.mkdirSync(path.join(copy, '.carrick'));
    fs.copyFileSync(path.join(first, '.carrick', 'typed.json'), path.join(copy, '.carrick', 'typed.json'));
    const own = await daemon(copy);
    assert.strictEqual(recordOf(copy)!.pid, own.pid);
    assert.strictEqual(recordOf(copy)!.workspace, fs.realpathSync(copy));
    assert.strictEqual(original.exitCode, null, 'the first daemon still serves its own workspace');
    await stopDaemon(own, recordOf(copy)!.socket);
    await stopDaemon(original, recordOf(first)!.socket);
  });

  it("stops on a first-frame stop carrying the record's token, from any build", async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const record = recordOf(ws)!;
    const wrong = await SocketClient.connect(record.socket);
    const refused = await wrong.send({ request_id: 's', action: 'stop', token: '0'.repeat(32) });
    assert.match(String((refused.errors as string[])[0]), /^build_mismatch/);
    await wrong.closed;
    assert.strictEqual(child.exitCode, null);

    const other = await SocketClient.connect(record.socket);
    const stopped = await other.send({ request_id: 's', action: 'stop', token: record.token });
    assert.strictEqual(stopped.status, 'success');
    assert.strictEqual(await exited(child), 0);
  });

  it('under its heap cap, a warm build never releases a service a probe used: it is marked cold_only:no_room', async () => {
    // The heap with service `a` held, measured on a daemon of its own.
    const measure = workspace();
    const measuring = await daemon(measure);
    const socket0 = recordOf(measure)!.socket;
    const w0 = await SocketClient.greeted(socket0);
    assert.strictEqual((await w0.send(initOf(measure, 'a'))).status, 'ready');
    assert.strictEqual((await w0.send(inferOf(measure, 'a'))).status, 'success');
    assert.strictEqual((await w0.send(captureOf(measure, 'a', fs.mkdtempSync(path.join(measure, 'stub-'))))).status, 'success');
    const heldA = (await statusOf(socket0)).heap_used_mb;
    w0.conn.destroy();
    await stopDaemon(measuring, socket0);
    // `a` fits; `a` and `b` together do not (a second program costs far more than the margin).
    const cap = String(heldA + 8);

    // A probe used `a`: the warm build of `b` releases `b` itself.
    const ws = workspace();
    const child = await daemon(ws, { CARRICK_TYPED_HEAP_CAP_MB: cap });
    const socket = recordOf(ws)!.socket;
    const warm = await SocketClient.greeted(socket);
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    assert.strictEqual((await warm.send(inferOf(ws, 'a'))).status, 'success');
    assert.strictEqual((await warm.send(captureOf(ws, 'a', fs.mkdtempSync(path.join(ws, 'stub-'))))).status, 'success');
    const probe = await SocketClient.greeted(socket, 'probe');
    assert.strictEqual((await probe.send(initOf(ws, 'a'))).status, 'ready');
    assert.strictEqual((await probe.send(inferOf(ws, 'a'))).status, 'success');
    assert.strictEqual((await warm.send(initOf(ws, 'b'))).status, 'ready');
    assert.strictEqual((await warm.send(inferOf(ws, 'b'))).status, 'success');
    let status = await statusOf(socket);
    assert.deepStrictEqual(
      [serviceIn(status, ws, 'a')!.state, serviceIn(status, ws, 'b')!.state],
      ['built', 'cold_only:no_room'],
      JSON.stringify(status)
    );
    assert.ok(status.errors.some((e) => e.kind === 'no_room' && e.service === 'b'), JSON.stringify(status.errors));
    assert.deepStrictEqual((await probe.send(initOf(ws, 'b'))).errors, ['not_held: cold_only:no_room']);
    assert.strictEqual(status.heap_cap_mb, Number(cap));
    assert.ok(status.heap_limit_mb > status.heap_cap_mb);
    warm.conn.destroy();
    probe.conn.destroy();
    await stopDaemon(child, socket);

    // No probe: the warm build of `b` releases `a`, the least recently used.
    const ws2 = workspace();
    const child2 = await daemon(ws2, { CARRICK_TYPED_HEAP_CAP_MB: cap });
    const socket2 = recordOf(ws2)!.socket;
    const warm2 = await SocketClient.greeted(socket2);
    assert.strictEqual((await warm2.send(initOf(ws2, 'a'))).status, 'ready');
    assert.strictEqual((await warm2.send(inferOf(ws2, 'a'))).status, 'success');
    assert.strictEqual((await warm2.send(captureOf(ws2, 'a', fs.mkdtempSync(path.join(ws2, 'stub-'))))).status, 'success');
    const firstFootprint = serviceIn(await statusOf(socket2), ws2, 'a')!.footprint_mb!;
    assert.strictEqual((await warm2.send(initOf(ws2, 'b'))).status, 'ready');
    assert.strictEqual((await warm2.send(inferOf(ws2, 'b'))).status, 'success');
    status = await statusOf(socket2);
    assert.deepStrictEqual([serviceIn(status, ws2, 'a')!.state, serviceIn(status, ws2, 'b')!.state], ['released', 'built'], JSON.stringify(status));
    assert.ok(status.errors.some((e) => e.kind === 'evicted' && e.service === 'a'), JSON.stringify(status.errors));
    // Its capture went with it: a probe capture is not built in the daemon.
    const probe2 = await SocketClient.greeted(socket2, 'probe');
    assert.strictEqual(await answerOrClose(probe2, captureOf(ws2, 'a', fs.mkdtempSync(path.join(ws2, 'stub-')))), 'closed');
    // Its next warm request builds it again, answering as before, and its footprint is measured afresh.
    const first = await warm2.send(initOf(ws2, 'a'));
    assert.strictEqual(first.status, 'ready');
    const rebuilt = await warm2.send(inferOf(ws2, 'a'));
    assert.strictEqual(rebuilt.status, 'success');
    assert.match(JSON.stringify(rebuilt.inferred_types), /id: string; n: number/);
    const again = serviceIn(await statusOf(socket2), ws2, 'a')!.footprint_mb!;
    // Measured afresh, never added to the footprint it had before its release.
    assert.ok(again <= firstFootprint, `footprint ${again} MB after a rebuild, ${firstFootprint} MB before`);
    warm2.conn.destroy();
    await stopDaemon(child2, socket2);
  });

  it('builds a service on its warm init, so a probe is served without another request', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const warm = await SocketClient.greeted(socket);
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    const status = await statusOf(socket);
    assert.strictEqual(serviceIn(status, ws, 'a')!.state, 'built', JSON.stringify(status.services));
    assert.ok((serviceIn(status, ws, 'a')!.footprint_mb ?? 0) > 0);
    const probe = await SocketClient.greeted(socket, 'probe');
    assert.strictEqual((await probe.send(initOf(ws, 'a'))).status, 'ready');
    assert.strictEqual((await probe.send(inferOf(ws, 'a'))).status, 'success');
    warm.conn.destroy();
    probe.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('forces a collection only for a warm build or over the cap, never for a probe under it', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const warm = await SocketClient.greeted(socket);
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    const afterWarm = (await statusOf(socket)).collections;
    assert.ok(afterWarm > 0, 'a warm build measures its footprint');
    const probe = await SocketClient.greeted(socket, 'probe');
    assert.strictEqual((await probe.send(initOf(ws, 'a'))).status, 'ready');
    for (let i = 0; i < 3; i += 1) assert.strictEqual((await probe.send(inferOf(ws, 'a'))).status, 'success');
    assert.strictEqual((await statusOf(socket)).collections, afterWarm);
    warm.conn.destroy();
    probe.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('removes the kept judge workspaces of dead daemons only', async () => {
    const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'clife-judges-'));
    workspaces.push(parent);
    const gone = spawn('node', ['-e', ''], { stdio: 'ignore' });
    await exited(gone);
    const dirs: Record<string, string | null> = { dead: String(gone.pid), live: String(process.pid), unmarked: null };
    for (const [name, owner] of Object.entries(dirs)) {
      fs.mkdirSync(path.join(parent, `carrick-check-v2-${name}`, 'node_modules'), { recursive: true });
      if (owner !== null) fs.writeFileSync(path.join(parent, `carrick-check-v2-${name}`, JUDGE_OWNER_FILE), `${owner}\n`);
    }
    // Not a check workspace, though it names a dead owner.
    fs.mkdirSync(path.join(parent, 'other-dead'));
    fs.writeFileSync(path.join(parent, 'other-dead', JUDGE_OWNER_FILE), `${gone.pid}\n`);
    assert.strictEqual(removeOrphanedJudgeWorkspaces(parent), 1);
    assert.deepStrictEqual(fs.readdirSync(parent).sort(), ['carrick-check-v2-live', 'carrick-check-v2-unmarked', 'other-dead']);
  });

  it('says warming while a warm connection is open', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    assert.strictEqual((await statusOf(socket)).warming, false);
    const warm = await SocketClient.greeted(socket, 'warm');
    assert.strictEqual((await statusOf(socket)).warming, true);
    warm.conn.destroy();
    await until('warming to end', async () => !(await statusOf(socket)).warming);
    await stopDaemon(child, socket);
  });
});
