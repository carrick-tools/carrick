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

/** A workspace that also holds service `big`: one file of `count` declarations, so its program costs far more than a small service's. */
function withBig(count: number): string {
  const ws = workspace();
  fs.mkdirSync(path.join(ws, 'big', 'src'), { recursive: true });
  fs.writeFileSync(path.join(ws, 'big', 'tsconfig.json'), TSCONFIG);
  const decls = Array.from({ length: count }, (_, i) => `export interface Shape${i} { id: string; n${i}: number; next?: Shape${(i + 1) % count} }`);
  fs.writeFileSync(path.join(ws, 'big', 'src', 'main.ts'), `${decls.join('\n')}\nexport function make() {\n  return { id: 'big', n: 1 };\n}\n`);
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
    // The heap with `a` held, and the footprints of `a` and then `b`, measured on a daemon of their own.
    const measure = workspace();
    const measuring = await daemon(measure);
    const socket0 = recordOf(measure)!.socket;
    const w0 = await SocketClient.greeted(socket0);
    assert.strictEqual((await w0.send(initOf(measure, 'a'))).status, 'ready');
    // Answered once, as the probe below answers it: the checker is part of what `a` holds.
    assert.strictEqual((await w0.send(inferOf(measure, 'a'))).status, 'success');
    const measured = await statusOf(socket0);
    const heldA = measured.heap_used_mb;
    const footprintA = serviceIn(measured, measure, 'a')!.footprint_mb!;
    assert.strictEqual((await w0.send(initOf(measure, 'b'))).status, 'ready');
    const footprintB = serviceIn(await statusOf(socket0), measure, 'b')!.footprint_mb!;
    w0.conn.destroy();
    await stopDaemon(measuring, socket0);
    // `a` fits, under half the cap, and leaves room to build (under 90% of it); `a` and `b` together do not fit; `b` is under half the cap.
    const capMb = Math.max(Math.ceil(heldA / 0.9), 2 * footprintA) + 4;
    const fixture = `heap with a ${heldA} MB, a ${footprintA} MB, b ${footprintB} MB, cap ${capMb} MB`;
    assert.ok(heldA + footprintB > capMb + 4 && footprintB <= capMb / 2, `fixture: ${fixture}`);

    const ws = workspace();
    const child = await daemon(ws, { CARRICK_TYPED_HEAP_CAP_MB: String(capMb) });
    const socket = recordOf(ws)!.socket;
    const warm = await SocketClient.greeted(socket);
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    const probe = await SocketClient.greeted(socket, 'probe');
    assert.strictEqual((await probe.send(initOf(ws, 'a'))).status, 'ready');
    assert.strictEqual((await probe.send(inferOf(ws, 'a'))).status, 'success');
    assert.strictEqual((await warm.send(initOf(ws, 'b'))).status, 'ready');
    const status = await statusOf(socket);
    assert.deepStrictEqual(
      [serviceIn(status, ws, 'a')!.state, serviceIn(status, ws, 'b')!.state],
      ['built', 'cold_only:no_room'],
      `${fixture}: ${JSON.stringify(status)}`
    );
    assert.ok(status.errors.some((e) => e.kind === 'no_room' && e.service === 'b'), JSON.stringify(status.errors));
    assert.deepStrictEqual((await probe.send(initOf(ws, 'b'))).errors, ['not_held: cold_only:no_room']);
    assert.strictEqual(status.heap_cap_mb, capMb);
    assert.ok(status.heap_limit_mb > status.heap_cap_mb);
    warm.conn.destroy();
    probe.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('without a probe, a warm build releases the least recently used service, whose footprint starts again from nothing', async () => {
    // The heap with `a` held, and the footprint of `big` built after it.
    const measure = withBig(6000);
    const measuring = await daemon(measure);
    const socket0 = recordOf(measure)!.socket;
    const w0 = await SocketClient.greeted(socket0);
    assert.strictEqual((await w0.send(initOf(measure, 'a'))).status, 'ready');
    const measured = await statusOf(socket0);
    const heldA = measured.heap_used_mb;
    const footprintA = serviceIn(measured, measure, 'a')!.footprint_mb!;
    assert.strictEqual((await w0.send(initOf(measure, 'big'))).status, 'ready');
    const footprintBig = serviceIn(await statusOf(socket0), measure, 'big')!.footprint_mb!;
    w0.conn.destroy();
    await stopDaemon(measuring, socket0);
    // Each fits alone (the process base plus its footprint), under half the cap; `a` leaves room to build (under 90% of it); together they do not fit.
    const base = heldA - footprintA;
    const capMb = Math.max(Math.ceil(heldA / 0.9), base + footprintBig, 2 * footprintA) + 6;
    const fixture = `heap with a ${heldA} MB, a ${footprintA} MB, big ${footprintBig} MB, cap ${capMb} MB`;
    assert.ok(heldA + footprintBig > capMb + 4 && footprintBig <= capMb / 2, `fixture: ${fixture}`);

    const ws = withBig(6000);
    const child = await daemon(ws, { CARRICK_TYPED_HEAP_CAP_MB: String(capMb) });
    const socket = recordOf(ws)!.socket;
    const warm = await SocketClient.greeted(socket);
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    const firstFootprint = serviceIn(await statusOf(socket), ws, 'a')!.footprint_mb!;
    assert.strictEqual((await warm.send(initOf(ws, 'big'))).status, 'ready');
    const status = await statusOf(socket);
    assert.deepStrictEqual([serviceIn(status, ws, 'a')!.state, serviceIn(status, ws, 'big')!.state], ['released', 'built'], `${fixture}: ${JSON.stringify(status)}`);
    assert.ok(status.errors.some((e) => e.kind === 'evicted' && e.service === 'a'), JSON.stringify(status.errors));
    // Released: its footprint starts again from nothing (a rebuild measures it afresh).
    assert.strictEqual(serviceIn(status, ws, 'a')!.footprint_mb, null);
    assert.ok(firstFootprint > 0);
    warm.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('marks a service whose footprint is over half the cap too_big, and keeps a small one built', async () => {
    // Footprints measured on a daemon of their own: `a` first, then `big`.
    const measure = withBig(24000);
    const measuring = await daemon(measure);
    const socket0 = recordOf(measure)!.socket;
    const w0 = await SocketClient.greeted(socket0);
    assert.strictEqual((await w0.send(initOf(measure, 'a'))).status, 'ready');
    const measured = await statusOf(socket0);
    const small = serviceIn(measured, measure, 'a')!.footprint_mb!;
    const heldSmall = measured.heap_used_mb;
    assert.strictEqual((await w0.send(initOf(measure, 'big'))).status, 'ready');
    const large = serviceIn(await statusOf(socket0), measure, 'big')!.footprint_mb!;
    w0.conn.destroy();
    await stopDaemon(measuring, socket0);
    // `a` is under half the cap, fits, and leaves room to build (under 90% of it); `big` is over half of it.
    const cap = Math.max(2 * small, Math.ceil(heldSmall / 0.9)) + 4;
    assert.ok(large > cap / 2, `fixture: big ${large} MB, small ${small} MB, cap ${cap} MB`);

    const ws = withBig(24000);
    const child = await daemon(ws, { CARRICK_TYPED_HEAP_CAP_MB: String(cap) });
    const socket = recordOf(ws)!.socket;
    const warm = await SocketClient.greeted(socket);
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    assert.strictEqual((await warm.send(initOf(ws, 'big'))).status, 'ready');
    const status = await statusOf(socket);
    assert.deepStrictEqual(
      [serviceIn(status, ws, 'a')!.state, serviceIn(status, ws, 'big')!.state],
      ['built', 'cold_only:too_big'],
      JSON.stringify({ cap, small, large, services: status.services, errors: status.errors })
    );
    assert.ok(status.errors.some((e) => e.kind === 'too_big' && e.service === 'big'), JSON.stringify(status.errors));
    warm.conn.destroy();
    await stopDaemon(child, socket);
  });

  it('closes a capture_v2 on any connection with no frame: the client captures in a process of its own', async () => {
    const ws = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    for (const mode of ['warm', 'probe'] as const) {
      const client = await SocketClient.greeted(socket, mode);
      assert.strictEqual(await answerOrClose(client, captureOf(ws, 'a', fs.mkdtempSync(path.join(ws, 'stub-')))), 'closed', mode);
    }
    assert.strictEqual((await statusOf(socket)).counters.closed_for_cold, 2);
    await stopDaemon(child, socket);
  });

  it('refuses a warm init as cold_only:no_room once the heap is past 90% of the cap, building nothing (carrick#2915)', async () => {
    // The heap with `a` held, measured on a daemon of its own.
    const measure = workspace();
    const measuring = await daemon(measure);
    const socket0 = recordOf(measure)!.socket;
    const w0 = await SocketClient.greeted(socket0);
    assert.strictEqual((await w0.send(initOf(measure, 'a'))).status, 'ready');
    const measured = await statusOf(socket0);
    const heldA = measured.heap_used_mb;
    const footprintA = serviceIn(measured, measure, 'a')!.footprint_mb!;
    w0.conn.destroy();
    await stopDaemon(measuring, socket0);
    // `a` fits (under the cap, under half of it) and leaves the heap past 90% of the cap.
    const cap = Math.floor(heldA / 0.95);
    assert.ok(heldA > 0.9 * cap + 2 && heldA < cap && footprintA <= cap / 2, `fixture: heap with a ${heldA} MB, a ${footprintA} MB, cap ${cap} MB`);

    const ws = workspace();
    const child = await daemon(ws, { CARRICK_TYPED_HEAP_CAP_MB: String(cap) });
    const socket = recordOf(ws)!.socket;
    const warm = await SocketClient.greeted(socket);
    assert.strictEqual((await warm.send(initOf(ws, 'a'))).status, 'ready');
    assert.deepStrictEqual((await warm.send(initOf(ws, 'b'))).errors, ['not_held: cold_only:no_room']);
    const status = await statusOf(socket);
    assert.strictEqual(serviceIn(status, ws, 'a')!.state, 'built', JSON.stringify(status.services));
    // Listed though never built, so the CLI reads it as cold_only, not as a service the daemon has not seen (carrick#2928).
    const refused = serviceIn(status, ws, 'b');
    assert.deepStrictEqual(refused && [refused.state, refused.built, refused.footprint_mb], ['cold_only:no_room', false, null], JSON.stringify(status.services));
    assert.deepStrictEqual(
      status.errors.map((e) => [e.kind, e.service]),
      [['no_room', 'b']],
      'nothing evicted, b refused'
    );
    const probe = await SocketClient.greeted(socket, 'probe');
    assert.deepStrictEqual((await probe.send(initOf(ws, 'b'))).errors, ['not_held: cold_only:no_room']);
    warm.conn.destroy();
    probe.conn.destroy();
    await stopDaemon(child, socket);
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

  it('removes the listed kept judge workspaces of dead daemons only, wherever they are', async () => {
    // A request's workspace_root, not the OS temp dir.
    const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'clife-judges-'));
    workspaces.push(parent);
    const root = path.join(parent, 'any', 'judge-root');
    const gone = spawn('node', ['-e', ''], { stdio: 'ignore' });
    await exited(gone);
    const dirs: Record<string, string | null> = { dead: String(gone.pid), live: String(process.pid), unmarked: null };
    for (const [name, owner] of Object.entries(dirs)) {
      fs.mkdirSync(path.join(root, `carrick-check-v2-${name}`, 'node_modules'), { recursive: true });
      if (owner !== null) fs.writeFileSync(path.join(root, `carrick-check-v2-${name}`, JUDGE_OWNER_FILE), `${owner}\n`);
    }
    // Not a check workspace, though it names a dead owner.
    fs.mkdirSync(path.join(root, 'other-dead'));
    fs.writeFileSync(path.join(root, 'other-dead', JUDGE_OWNER_FILE), `${gone.pid}\n`);
    const listed = [...Object.keys(dirs).map((name) => path.join(root, `carrick-check-v2-${name}`)), path.join(root, 'other-dead'), path.join(root, 'carrick-check-v2-gone')];
    const outcome = removeOrphanedJudgeWorkspaces(listed);
    assert.deepStrictEqual(outcome, { removed: 1, live: [path.join(root, 'carrick-check-v2-live')] });
    assert.deepStrictEqual(fs.readdirSync(root).sort(), ['carrick-check-v2-live', 'carrick-check-v2-unmarked', 'other-dead']);
  });

  it('on start, removes the dead kept judge workspaces its record lists and keeps the list to the live ones', async () => {
    const ws = workspace();
    const gone = spawn('node', ['-e', ''], { stdio: 'ignore' });
    await exited(gone);
    const judges = fs.mkdtempSync(path.join(os.tmpdir(), 'clife-root-'));
    workspaces.push(judges);
    const dead = path.join(judges, 'carrick-check-v2-dead');
    fs.mkdirSync(path.join(dead, 'node_modules'), { recursive: true });
    fs.writeFileSync(path.join(dead, JUDGE_OWNER_FILE), `${gone.pid}\n`);
    fs.mkdirSync(path.join(ws, '.carrick'));
    fs.writeFileSync(path.join(ws, '.carrick', 'typed.judges'), `${dead}\n`);
    const child = await daemon(ws);
    await until('the dead judge workspace to go', () => !fs.existsSync(dead));
    assert.strictEqual(fs.readFileSync(path.join(ws, '.carrick', 'typed.judges'), 'utf8'), '');
    await stopDaemon(child, recordOf(ws)!.socket);
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
