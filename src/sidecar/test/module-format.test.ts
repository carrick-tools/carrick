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
