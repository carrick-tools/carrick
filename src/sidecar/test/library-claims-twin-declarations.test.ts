/**
 * carrick#4932: a package can ship twin declarations, `index.d.mts` for an
 * ESM importer and `index.d.cts` for a CommonJS one. The verifier must read
 * the package as the service's own files are typed, so a verified verdict
 * names the same declaration file and line as the call's position row.
 *
 * A service whose tsconfig is a solution config (`"files": []` plus
 * references) carries no compiler options of its own: its files are typed
 * under the referenced project that owns them. The verifier read the package
 * under the solution config's empty options (Node10 resolution, the
 * package's `types` field, the `.d.cts` twin) while the rows named the twin
 * the owner resolves.
 *
 * The package is invented. Its two twins differ by one leading line, so a
 * wrong twin names a wrong line as well as a wrong file.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { SidecarClient } from './helpers.js';

const P = '@fixture/twin';
const SERVER = `export type Handler = (request: unknown, reply: unknown) => void;
export declare class Server {
  constructor();
  post(path: string, handler: Handler): this;
}
`;
const SERVICE = `import { Server } from '${P}';
const app = new Server();
app.post('/b', () => {});
`;

type Row = [string, number, number, string | null, string | null, number | null, string | null, string | null, number];
interface Verdict {
  verdict: string;
  reason?: string;
  declarations?: Array<{ decl_file: string; decl_line: number }>;
}

interface Layout {
  /** The service's own package.json fields beside its name. */
  pkg: Record<string, unknown>;
  /** The options of the config that owns `src`. */
  options: Record<string, unknown>;
  /** Whether the root tsconfig is a solution config referencing the owner. */
  solution: boolean;
  /** A second referenced project's options, owning only `tool.config.ts`. */
  tool?: Record<string, unknown>;
}

/** The post op's verdict and the call row of `app.post`, for one layout. */
async function verdictAndRow(layout: Layout): Promise<{ verdict: Verdict; row: Row; channel: Verdict }> {
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-twin-')));
  const store = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-claims-twin-rows-')));
  const client = new SidecarClient();
  try {
    const write = (rel: string, text: string) => {
      const target = path.join(root, rel);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, text);
    };
    const owner = { compilerOptions: { strict: true, skipLibCheck: true, types: [], ...layout.options }, include: ['src'] };
    if (layout.solution) {
      const references = [{ path: './tsconfig.app.json' }, ...(layout.tool ? [{ path: './tsconfig.tool.json' }] : [])];
      write('tsconfig.json', JSON.stringify({ files: [], references }));
      write('tsconfig.app.json', JSON.stringify(owner));
      if (layout.tool) {
        write('tsconfig.tool.json', JSON.stringify({ compilerOptions: { skipLibCheck: true, types: [], ...layout.tool }, include: ['tool.config.ts'] }));
        write('tool.config.ts', 'export const tool = 1;\n');
      }
    } else {
      write('tsconfig.json', JSON.stringify(owner));
    }
    write('package.json', JSON.stringify({ name: 'service', ...layout.pkg }));
    write('src/index.ts', SERVICE);
    write(
      `node_modules/${P}/package.json`,
      JSON.stringify({
        name: P,
        version: '1.0.0',
        main: './dist/index.cjs',
        types: './dist/index.d.cts',
        exports: {
          '.': {
            import: { types: './dist/index.d.mts', default: './dist/index.mjs' },
            require: { types: './dist/index.d.cts', default: './dist/index.cjs' },
          },
        },
      })
    );
    write(`node_modules/${P}/dist/index.d.mts`, SERVER);
    write(`node_modules/${P}/dist/index.d.cts`, `// CommonJS twin\n${SERVER}`);

    await client.start();
    const ready = await client.send<{ status: string }>({
      request_id: 'init',
      action: 'init',
      repo_root: root,
      tsconfig_path: 'tsconfig.json',
      scan_root: root,
    });
    assert.strictEqual(ready.status, 'ready');
    const out = path.join(store, 'rows.jsonl.gz');
    const walked = await client.send<{ status: string; errors?: string[] }>(
      { action: 'position_rows', request_id: 'rows', files: [path.join(root, 'src/index.ts')], scan_root: root, out_path: out, service: 'svc' },
      60_000
    );
    assert.strictEqual(walked.status, 'success', JSON.stringify(walked.errors));
    const lines = zlib.gunzipSync(fs.readFileSync(out)).toString('utf8').trimEnd().split('\n').slice(1);
    const rows = (JSON.parse(lines[0]) as { r: Row[] }).r.filter(r => r[0] === 'call' && r[3] === 'post');
    assert.strictEqual(rows.length, 1, 'one call row for post');
    const check = (id: string, receiver: string, claim: Record<string, unknown>) => ({
      claim_id: id,
      package: P,
      export: 'Server',
      role: 'server_framework',
      receiver,
      claim,
    });
    const response = await client.send<{ status: string; verdicts?: Verdict[]; errors?: string[] }>(
      {
        request_id: 'claims',
        action: 'verify_library_claims',
        from_dir: root,
        scan_root: root,
        checks: [
          check('m0', 'export', { kind: 'make', form: 'new', member: null }),
          check('post', 'instance:new', { kind: 'op', op: 'receive', member: 'post', name: { arg: 0 }, of: 'instance:new' }),
          {
            claim_id: 'channel',
            package: 'lib:lib.dom.d.ts',
            export: 'BroadcastChannel',
            role: 'in_process_bus',
            receiver: 'export',
            claim: { kind: 'make', form: 'new', member: null, name: { arg: 0 } },
          },
        ],
      },
      60_000
    );
    assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
    return { verdict: response.verdicts![1], row: rows[0], channel: response.verdicts![2] };
  } finally {
    await client.stop();
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(store, { recursive: true, force: true });
  }
}

const twin = (ext: string) => `node_modules/${P}/dist/index.${ext}`;

/** The verdict names exactly the declaration the row names, in the `ext` twin. */
function assertJoined({ verdict, row }: { verdict: Verdict; row: Row }, ext: string): void {
  assert.strictEqual(row[4], twin(ext), 'the position row names the twin the owner resolves');
  assert.strictEqual(verdict.verdict, 'verified', JSON.stringify(verdict));
  assert.deepStrictEqual(verdict.declarations, [{ decl_file: row[4], decl_line: row[5] }]);
}

describe('verify_library_claims: twin .d.mts/.d.cts read as the service reads them (carrick#4932)', () => {
  const bundler = { module: 'esnext', moduleResolution: 'bundler', target: 'es2022' };
  const nodenext = { module: 'nodenext', moduleResolution: 'nodenext', target: 'es2022' };

  it('a solution config whose owner resolves `bundler` names the .d.mts twin, as the row does', async () => {
    assertJoined(await verdictAndRow({ pkg: {}, options: bundler, solution: true }), 'd.mts');
  });

  it('a solution config whose owner is `nodenext` under "type": "module" names the .d.mts twin', async () => {
    assertJoined(await verdictAndRow({ pkg: { type: 'module' }, options: nodenext, solution: true }), 'd.mts');
  });

  it('a solution config whose owner is a CommonJS `nodenext` service names the .d.cts twin', async () => {
    assertJoined(await verdictAndRow({ pkg: {}, options: nodenext, solution: true }), 'd.cts');
  });

  it('two owners resolving differently each name their own twin, the row\'s among them', async () => {
    const { verdict, row } = await verdictAndRow({
      pkg: {},
      options: bundler,
      solution: true,
      tool: { module: 'commonjs', moduleResolution: 'node10', target: 'es2020' },
    });
    assert.strictEqual(row[4], twin('d.mts'));
    assert.strictEqual(verdict.verdict, 'verified', JSON.stringify(verdict));
    assert.deepStrictEqual(verdict.declarations, [
      { decl_file: twin('d.cts'), decl_line: row[5]! + 1 },
      { decl_file: twin('d.mts'), decl_line: row[5] },
    ]);
  });

  it('a claim only a later owner can read (its lib has the DOM, the first\'s has not) is verified', async () => {
    const { channel } = await verdictAndRow({
      pkg: {},
      options: { ...bundler, lib: ['es2022'] },
      solution: true,
      tool: { ...bundler, lib: ['es2022', 'dom'] },
    });
    assert.strictEqual(channel.verdict, 'verified', JSON.stringify(channel));
  });

  it('a single `bundler` config, as before, names the .d.mts twin', async () => {
    assertJoined(await verdictAndRow({ pkg: {}, options: bundler, solution: false }), 'd.mts');
  });

  it('a single CommonJS `node10` config, as before, names the .d.cts twin', async () => {
    assertJoined(
      await verdictAndRow({ pkg: {}, options: { module: 'commonjs', moduleResolution: 'node10', target: 'es2020' }, solution: false }),
      'd.cts'
    );
  });
});
