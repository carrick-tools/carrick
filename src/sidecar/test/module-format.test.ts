/**
 * carrick#1619: the init'd project resolved every import as a `require`.
 *
 * Under `module` `node16`..`nodenext` the compiler picks an import's export
 * conditions from the importing file's own format: its nearest `package.json`
 * `"type"`, or its extension. ts-morph creates each file without one, so every
 * import in `infer`, `bundle` and `retype_check` resolved with the `require`
 * conditions, and a package whose `exports` entry only has an `import` branch
 * did not resolve at all. The capture uses the compiler directly and read the
 * same import fine, so the two disagreed on one service: `infer` printed the
 * unresolved name, and a surface anchor built from that text dangled.
 *
 * The fixture is the workspace shape that found it: a sibling package whose
 * types resolve to source only through a custom `import` condition. The
 * CommonJS consumer is the control: the compiler resolves its import with the
 * `require` conditions and finds nothing, so it must stay unresolved here too.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    target: 'ES2022',
    module: 'NodeNext',
    moduleResolution: 'NodeNext',
    customConditions: ['@acme/source'],
    strict: true,
    skipLibCheck: true,
  },
  include: ['src'],
});

const CLIENT_TS = `import type { Me } from "@acme/core/types";

export interface Session { me: Me; expiresAt: string }

export async function me(): Promise<Me> {
  const res = await fetch("/api/v1/me");
  return (await res.json()) as Me;
}
`;
/** 1-based line of `export async function me()` in CLIENT_TS. */
const ME_FN_LINE = 5;

const tempRoots: string[] = [];

after(() => {
  for (const root of tempRoots) fs.rmSync(root, { recursive: true, force: true });
});

/**
 * A workspace whose `@acme/core` exports its types only under `import`, linked
 * into `node_modules` the way a workspace install links it. `type` is the
 * consumer package's module format. Returns the consumer's directory.
 */
function workspace(type: 'module' | 'commonjs'): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1619-'));
  tempRoots.push(root);
  const files: Record<string, string> = {
    'packages/core/package.json': JSON.stringify({
      name: '@acme/core',
      version: '1.0.0',
      type: 'module',
      exports: {
        './types': {
          import: { '@acme/source': './src/types.ts', types: './dist/types.d.ts' },
        },
      },
    }),
    'packages/core/src/types.ts': 'export type Me = { id: string; email: string };\n',
    'packages/client/package.json': JSON.stringify({ name: '@acme/client', version: '1.0.0', type }),
    'packages/client/tsconfig.json': TSCONFIG,
    'packages/client/src/client.ts': CLIENT_TS,
    // An `.mts` file is an ES module whatever its package says.
    'packages/client/src/client.mts': CLIENT_TS,
  };
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  fs.mkdirSync(path.join(root, 'node_modules', '@acme'), { recursive: true });
  fs.symlinkSync(
    path.join('..', '..', 'packages', 'core'),
    path.join(root, 'node_modules', '@acme', 'core'),
    'dir'
  );
  return path.join(root, 'packages', 'client');
}

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string; any_provenance?: unknown[] }>;
}
interface BundleShape {
  dts_content?: string;
  symbol_failures?: unknown[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

async function read(
  service: string,
  file = 'client.ts'
): Promise<{ ret: { type_string: string; any_provenance?: unknown[] }; bundle: string }> {
  const client = new SidecarClient();
  await client.start();
  try {
    await client.send({ action: 'init', request_id: 'init', repo_root: service, tsconfig_path: 'tsconfig.json' });
    const inferred = await client.send<InferShape>(
      {
        action: 'infer',
        request_id: 'me',
        requests: [
          {
            file_path: path.join(service, 'src', file),
            line_number: ME_FN_LINE,
            infer_kind: 'function_return',
            alias: 'Me_Return',
          },
        ],
      },
      30000
    );
    const ret = inferred.inferred_types?.find((t) => t.alias === 'Me_Return');
    assert.ok(ret, `no inference: ${JSON.stringify(inferred)}`);
    const bundled = await client.send<BundleShape>(
      {
        action: 'bundle',
        request_id: 'session',
        symbols: [{ symbol_name: 'Session', source_file: `src/${file}`, alias: 'Session_Body' }],
      },
      30000
    );
    return { ret, bundle: collapse(bundled.dts_content ?? '') };
  } finally {
    await client.stop();
  }
}

describe('carrick#1619: an import resolves in the mode of the file that imports it', () => {
  it('follows an import-only export from an ES module consumer', async () => {
    const { ret, bundle } = await read(workspace('module'));
    assert.strictEqual(collapse(ret.type_string), '{ id: string; email: string; }');
    assert.strictEqual(ret.any_provenance, undefined);
    assert.match(bundle, /Session_Body \{ me: \{ id: string; email: string; \}; expiresAt: string; \}/);
  });

  it('follows it from an `.mts` file inside a CommonJS package', async () => {
    const { ret, bundle } = await read(workspace('commonjs'), 'client.mts');
    assert.strictEqual(collapse(ret.type_string), '{ id: string; email: string; }');
    assert.match(bundle, /Session_Body \{ me: \{ id: string; email: string; \}; expiresAt: string; \}/);
  });

  it('leaves the same import unresolved from a CommonJS consumer, as the compiler does', async () => {
    // The compiler resolves a CommonJS file's import with the `require`
    // conditions; the export has no `require` branch, so nothing resolves.
    const { ret } = await read(workspace('commonjs'));
    assert.strictEqual(ret.type_string, 'Me');
    assert.ok(ret.any_provenance, 'an unresolved return must carry its provenance');
  });
});

// ---------------------------------------------------------------------------
// One file importing one package in two modes (review of the first fix).
// ---------------------------------------------------------------------------

/** Each import resolves in its own mode: `resolution-mode` first, then plain. */
const MIXED_TS = `import type { Shape as ReqShape } from "dual" with { "resolution-mode": "require" };
import { v } from "dual";
export function req(r: ReqShape) { return { ...r }; }
export function imp() { return { ...v }; }
`;
/** The same two imports in the other order. */
const MIXED_REV_TS = `import { v } from "dual";
import type { Shape as ReqShape } from "dual" with { "resolution-mode": "require" };
export function imp() { return { ...v }; }
export function req(r: ReqShape) { return { ...r }; }
`;
/** A CommonJS file that requires the package and also imports it dynamically. */
const MIXED_CTS = `import dualReq = require("dual");
export function viaReq() { return { ...dualReq.v }; }
export async function viaDyn() { const m = await import("dual"); return { ...m.v }; }
`;

const CJS_SHAPE = '{ id: number; cjs: true; }';
const ESM_SHAPE = '{ id: string; esm: true; }';

/** An ES module service over a package with separate import and require types. */
function dualService(
  compilerOptions: Record<string, unknown>,
  withCts = true,
  extra: Record<string, string> = {}
): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1619-mixed-'));
  tempRoots.push(root);
  const files: Record<string, string> = {
    'package.json': JSON.stringify({ name: 'svc', version: '1.0.0', type: 'module' }),
    'tsconfig.json': JSON.stringify({
      compilerOptions: { strict: true, skipLibCheck: true, target: 'ES2022', ...compilerOptions },
      include: ['src'],
    }),
    'node_modules/dual/package.json': JSON.stringify({
      name: 'dual',
      version: '1.0.0',
      exports: { '.': { import: { types: './esm.d.mts' }, require: { types: './cjs.d.cts' } } },
    }),
    'node_modules/dual/esm.d.mts': 'export interface Shape { id: string; esm: true }\nexport declare const v: Shape;\n',
    'node_modules/dual/cjs.d.cts': 'export interface Shape { id: number; cjs: true }\nexport declare const v: Shape;\n',
    'src/mixed.ts': MIXED_TS,
    'src/mixed-rev.ts': MIXED_REV_TS,
    ...(withCts ? { 'src/mixed-cjs.cts': MIXED_CTS } : {}),
    ...extra,
  };
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return root;
}

/** `function_return` of each named function, keyed `file:function`. */
async function returns(service: string, probes: Array<[string, string, number]>): Promise<Map<string, string>> {
  const client = new SidecarClient();
  await client.start();
  try {
    await client.send({ action: 'init', request_id: 'init', repo_root: service, tsconfig_path: 'tsconfig.json' });
    const res = await client.send<InferShape>(
      {
        action: 'infer',
        request_id: 'mixed',
        requests: probes.map(([file, fn, line]) => ({
          file_path: path.join(service, 'src', file),
          line_number: line,
          infer_kind: 'function_return',
          alias: `${file}:${fn}`,
        })),
      },
      30000
    );
    return new Map((res.inferred_types ?? []).map((t) => [t.alias, collapse(t.type_string)]));
  } finally {
    await client.stop();
  }
}

describe('carrick#1619: two imports of one package in one file keep their own modes', () => {
  it('under NodeNext', async () => {
    const got = await returns(dualService({ module: 'NodeNext', moduleResolution: 'NodeNext' }), [
      ['mixed.ts', 'req', 3],
      ['mixed.ts', 'imp', 4],
      ['mixed-rev.ts', 'imp', 3],
      ['mixed-rev.ts', 'req', 4],
      ['mixed-cjs.cts', 'viaReq', 2],
      ['mixed-cjs.cts', 'viaDyn', 3],
    ]);
    assert.strictEqual(got.get('mixed.ts:req'), CJS_SHAPE);
    assert.strictEqual(got.get('mixed.ts:imp'), ESM_SHAPE);
    assert.strictEqual(got.get('mixed-rev.ts:imp'), ESM_SHAPE);
    assert.strictEqual(got.get('mixed-rev.ts:req'), CJS_SHAPE);
    assert.strictEqual(got.get('mixed-cjs.cts:viaReq'), CJS_SHAPE);
    assert.strictEqual(got.get('mixed-cjs.cts:viaDyn'), ESM_SHAPE);
  });

  it('under Bundler', async () => {
    // No `.cts` file here: under Bundler this TypeScript copy keeps one cache
    // entry per package across modes, so a `.cts` that requires the package
    // first fixes the answer for every later file (carrick#1632).
    const got = await returns(dualService({ module: 'ESNext', moduleResolution: 'Bundler' }, false), [
      ['mixed.ts', 'req', 3],
      ['mixed.ts', 'imp', 4],
      ['mixed-rev.ts', 'imp', 3],
      ['mixed-rev.ts', 'req', 4],
    ]);
    assert.strictEqual(got.get('mixed.ts:req'), CJS_SHAPE);
    assert.strictEqual(got.get('mixed.ts:imp'), ESM_SHAPE);
    assert.strictEqual(got.get('mixed-rev.ts:imp'), ESM_SHAPE);
    assert.strictEqual(got.get('mixed-rev.ts:req'), CJS_SHAPE);
  });

  it('leaves a file format unset outside node16..nodenext', async () => {
    // Under Bundler an `import` in a `.cts` file resolves in import mode when
    // the file has no format, as before. Given its CommonJS format it would
    // resolve in require mode first, and this TypeScript copy would then serve
    // that answer to every later file (carrick#1632).
    const PLAIN = 'import { v } from "dual";\nexport function dualV() { return { ...v }; }\n';
    const got = await returns(
      dualService({ module: 'ESNext', moduleResolution: 'Bundler' }, false, { 'src/a.cts': PLAIN, 'src/esm.ts': PLAIN }),
      [['esm.ts', 'dualV', 2]]
    );
    assert.strictEqual(got.get('esm.ts:dualV'), ESM_SHAPE);
  });
});
