/**
 * carrick#1660: the surface lister as a released artifact. `carrick-lister.mjs`
 * is the built sidecar in one file, run by the cloud's library store on its
 * Node 22 runtime with a bare environment, against a directory holding one
 * package and its installed type closure.
 *
 * By default the test builds the artifact itself, which needs the build's
 * own install (`npm ci --prefix lister`); it sits outside `dist/test/*.test.js`
 * so the sidecar suite does not need it. The release workflow sets
 * CARRICK_LISTER_ARTIFACT to the entry it is about to attach, so the bytes
 * tested are the bytes released.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import { spawn } from 'node:child_process';
import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as readline from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { SIDECAR_PATH } from '../helpers.js';

const here = path.dirname(fileURLToPath(import.meta.url));
// dist/test/lister/ -> the sidecar root.
const BUILD_SCRIPT = path.join(here, '..', '..', '..', 'lister', 'build.mjs');

interface Manifest {
  tag: string;
  source_sha: string;
  entry: string;
  node_floor: string;
  protocol: string;
  files: Record<string, string>;
}

interface BuildLister {
  buildLister(options: { tag: string; sourceSha: string; outDir: string }): Promise<Manifest>;
}

const sha256 = (file: string) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');

// A package whose declarations only read right with the default library:
// `NonNullable` and `Promise` are declared there, not by the package.
const QUEUE = `export interface Message {
  id: string;
}
export interface Queue {
  publish(topic: NonNullable<string | null>, body: unknown): Promise<Message>;
  subscribe(topic: string, handler: (message: Message) => void): void;
}
export declare function connect(options: { url: string; name?: string }): Queue;
`;

/** The directory the store hands the lister: one package installed, a probe tsconfig. */
function packageDirectory(): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-lister-pkg-'));
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'probe', private: true, type: 'module', dependencies: { 'fixture-queue': '1.0.0' } }),
    'tsconfig.json': JSON.stringify({
      compilerOptions: { target: 'ES2022', module: 'NodeNext', moduleResolution: 'NodeNext', strict: true, skipLibCheck: true, noEmit: true },
      include: ['src'],
    }),
    'src/index.ts': 'export {};\n',
    'node_modules/fixture-queue/package.json': JSON.stringify({ name: 'fixture-queue', version: '1.0.0', types: 'index.d.ts' }),
    'node_modules/fixture-queue/index.d.ts': QUEUE,
  };
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
    fs.writeFileSync(path.join(dir, rel), text);
  }
  return dir;
}

interface Run {
  init: { status: string };
  listed: {
    status: string;
    surfaces: Array<{
      package: string;
      reason?: string;
      exports: Array<{
        export: string;
        receivers: Array<{ receiver: string; members: Array<{ name: string; signatures: Array<{ params: Array<{ name: string; accepts_string: boolean }>; returns: string }> }> }>;
      }>;
    }>;
    surface_sha256: string;
  };
  stderr: string;
}

/**
 * Speak the protocol the way the store's adapter does: a child with only
 * PATH, LANG and TZ, `cwd` the package directory, `init` then
 * `list_library_surface`, progress frames skipped; then `shutdown`, so an
 * exit hook can run.
 */
async function runLister(entry: string, dir: string, nodeArgs: string[] = [], extraEnv: Record<string, string> = {}): Promise<Run> {
  const env: Record<string, string> = { ...extraEnv };
  for (const key of ['PATH', 'LANG', 'TZ']) if (process.env[key]) env[key] = process.env[key]!;
  const child = spawn(process.execPath, [...nodeArgs, entry], { cwd: dir, env, stdio: ['pipe', 'pipe', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (data: Buffer) => (stderr += data.toString()));
  const waiting = new Map<string, { resolve: (message: unknown) => void; reject: (err: Error) => void }>();
  readline.createInterface({ input: child.stdout }).on('line', line => {
    const message = JSON.parse(line) as { status?: string; request_id?: string };
    if (message.status === 'progress') return;
    waiting.get(message.request_id ?? '')?.resolve(message);
  });
  // A lister that dies (or is killed below) fails what it has not answered, with its log.
  const exited = new Promise<number | null>(resolve =>
    child.on('exit', code => {
      for (const { reject } of waiting.values()) reject(new Error(`the lister exited (${code}) before answering:\n${stderr.slice(-2000)}`));
      waiting.clear();
      resolve(code);
    })
  );
  const send = <T>(body: Record<string, unknown>) =>
    new Promise<T>((resolve, reject) => {
      waiting.set(body.request_id as string, { resolve: resolve as (message: unknown) => void, reject });
      child.stdin.write(`${JSON.stringify(body)}\n`);
    });
  const timer = setTimeout(() => child.kill('SIGKILL'), 60_000);
  try {
    const init = await send<Run['init']>({ request_id: 'init', action: 'init', repo_root: dir });
    const listed = await send<Run['listed']>({
      request_id: 'list',
      action: 'list_library_surface',
      from_dir: dir,
      packages: ['fixture-queue', 'node:events'],
      max_entries: 1_000_000,
    });
    child.stdin.write(`${JSON.stringify({ request_id: 'bye', action: 'shutdown' })}\n`);
    await exited;
    return { init, listed, stderr };
  } finally {
    clearTimeout(timer);
    child.kill('SIGKILL');
  }
}

describe('the surface lister artifact (carrick#1660)', () => {
  const scratch: string[] = [];
  const temp = (prefix: string) => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
    scratch.push(dir);
    return dir;
  };
  let builder: BuildLister;
  let manifest: Manifest;
  let artifactDir: string;
  let entry: string;
  let pkgDir: string;

  before(async () => {
    try {
      builder = (await import(pathToFileURL(BUILD_SCRIPT).href)) as BuildLister;
    } catch (err) {
      throw new Error(`cannot load ${BUILD_SCRIPT}; install the bundler with \`npm ci --prefix lister\`: ${String(err)}`);
    }
    const released = process.env.CARRICK_LISTER_ARTIFACT;
    if (released) {
      artifactDir = path.dirname(path.resolve(released));
      manifest = JSON.parse(fs.readFileSync(path.join(artifactDir, 'carrick-lister.manifest.json'), 'utf8')) as Manifest;
    } else {
      artifactDir = temp('carrick-lister-build-');
      manifest = await builder.buildLister({ tag: 'carrick-v0.0.0-test', sourceSha: 'test-sha', outDir: artifactDir });
    }
    // The entry alone, in a directory with nothing else in it.
    const isolated = temp('carrick-lister-alone-');
    entry = path.join(isolated, manifest.entry);
    fs.copyFileSync(path.join(artifactDir, manifest.entry), entry);
    pkgDir = packageDirectory();
    scratch.push(pkgDir);
  });

  after(() => {
    for (const dir of scratch) fs.rmSync(dir, { recursive: true, force: true });
  });

  it('writes a manifest naming the entry, its source and every file by sha256', () => {
    assert.deepStrictEqual(Object.keys(manifest).sort(), ['entry', 'files', 'node_floor', 'protocol', 'source_sha', 'tag']);
    assert.strictEqual(manifest.entry, 'carrick-lister.mjs');
    assert.strictEqual(manifest.node_floor, '22');
    assert.strictEqual(manifest.protocol, 'sidecar-stdio');
    assert.ok(manifest.tag.length > 0 && manifest.source_sha.length > 0);
    if (!process.env.CARRICK_LISTER_ARTIFACT) {
      assert.strictEqual(manifest.tag, 'carrick-v0.0.0-test');
      assert.strictEqual(manifest.source_sha, 'test-sha');
    }
    // Flat: one file, the entry, hashed as the release serves it.
    assert.deepStrictEqual(Object.keys(manifest.files), [manifest.entry]);
    assert.strictEqual(manifest.files[manifest.entry], sha256(path.join(artifactDir, manifest.entry)));
  });

  it('builds the same bytes again, from any working directory', async () => {
    // The release builds in a fresh checkout; the command line, run from elsewhere, must agree.
    const outDir = temp('carrick-lister-again-');
    const cli = spawn(
      process.execPath,
      [BUILD_SCRIPT, '--tag', manifest.tag, '--source-sha', manifest.source_sha, '--out', outDir],
      { cwd: os.tmpdir(), stdio: ['ignore', 'pipe', 'pipe'] }
    );
    let output = '';
    cli.stdout.on('data', (data: Buffer) => (output += data.toString()));
    cli.stderr.on('data', (data: Buffer) => (output += data.toString()));
    const code = await new Promise<number | null>(resolve => cli.on('exit', resolve));
    assert.strictEqual(code, 0, output);
    const again = JSON.parse(fs.readFileSync(path.join(outDir, 'carrick-lister.manifest.json'), 'utf8')) as Manifest;
    assert.deepStrictEqual(again, manifest);
  });

  it('answers init with ready and lists a package with the default library, alone and with a bare environment', async () => {
    const run = await runLister(entry, pkgDir);
    assert.strictEqual(run.init.status, 'ready', run.stderr);
    assert.strictEqual(run.listed.status, 'success', run.stderr);
    const queue = run.listed.surfaces[0];
    assert.strictEqual(queue.reason, undefined);
    const connect = queue.exports.find(e => e.export === 'connect')!;
    const instance = connect.receivers.find(r => r.receiver === 'instance:()')!;
    const publish = instance.members.find(m => m.name === 'publish')!;
    // `NonNullable` resolves only through the default library.
    assert.strictEqual(publish.signatures[0].params[0].accepts_string, true);
    assert.strictEqual(publish.signatures[0].returns, 'Promise<Message>');
    // No runtime types package is installed, so the runtime module lists nothing.
    assert.strictEqual(run.listed.surfaces[1].reason, 'module_unresolved');
  });

  it('lists exactly what the unbundled sidecar lists', async () => {
    const bundled = await runLister(entry, pkgDir);
    const unbundled = await runLister(SIDECAR_PATH, pkgDir);
    assert.deepStrictEqual(bundled.listed.surfaces, unbundled.listed.surfaces);
    assert.strictEqual(bundled.listed.surface_sha256, unbundled.listed.surface_sha256);
  });

  it('reads no file outside its own and the package directory', async () => {
    const traceDir = temp('carrick-lister-trace-');
    const tracer = path.join(traceDir, 'trace.mjs');
    const out = path.join(traceDir, 'reads.json');
    // Every content read that succeeded: files read and directories listed.
    fs.writeFileSync(
      tracer,
      `import fs from 'node:fs';
import { syncBuiltinESMExports } from 'node:module';
const reads = [];
const record = (target) => { if (typeof target === 'string') reads.push(target); else if (target instanceof URL) reads.push(target.pathname); };
for (const name of ['readFileSync', 'readdirSync', 'openSync', 'opendirSync']) {
  const original = fs[name];
  fs[name] = function (target, ...rest) { const result = original.call(this, target, ...rest); record(target); return result; };
}
for (const name of ['readFile', 'readdir', 'open', 'opendir']) {
  const original = fs.promises[name];
  fs.promises[name] = async function (target, ...rest) { const result = await original.call(this, target, ...rest); record(target); return result; };
}
syncBuiltinESMExports();
process.on('exit', () => fs.writeFileSync(${JSON.stringify(out)}, JSON.stringify(reads)));
`
    );
    const run = await runLister(entry, pkgDir, ['--import', pathToFileURL(tracer).href]);
    assert.strictEqual(run.listed.status, 'success', run.stderr);
    const reads = JSON.parse(fs.readFileSync(out, 'utf8')) as string[];
    assert.ok(reads.some(read => read.endsWith(path.join('fixture-queue', 'index.d.ts'))), 'the trace saw no read');
    // Node's loader reads the entry itself through the same module.
    const own = new Set([entry, fs.realpathSync(entry)]);
    const roots = [pkgDir, fs.realpathSync(pkgDir)];
    const outside = reads
      .map(read => path.resolve(read))
      .filter(read => !own.has(read) && !roots.some(root => read === root || read.startsWith(root + path.sep)));
    assert.deepStrictEqual(outside, []);
  });
});
