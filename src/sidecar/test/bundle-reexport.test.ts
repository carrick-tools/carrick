/**
 * carrick#1605: a symbol the model names through a barrel failed the bundle's
 * symbol check.
 *
 * The model reports a type where the consumer imports it from, and a consumer
 * usually imports from a package's index: `import type { Me } from
 * "./core/index.js"`, where the index only has `export * from ...`. The bundle
 * looked for a declaration of that name in the named file itself, so every
 * such row logged `Symbol 'Me' not found` and lost its explicit anchor.
 *
 * The name is now followed the way an import of it is: through `export *`
 * (any depth), `export { X } from` and `export { X as Y } from`, to the
 * declaration it resolves to. A name two `export *` sources both provide is
 * ambiguous (TypeScript reports TS2308 and an ES module exports neither), so
 * it stays a symbol failure rather than a silent pick of the first.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const FILES: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'NodeNext',
      moduleResolution: 'NodeNext',
      strict: true,
      skipLibCheck: true,
    },
    include: ['src'],
  }),
  'src/core/api.ts': [
    'export type Me = { id: string; email: string };',
    'export interface Account { id: string; plan: "basic" | "pro" }',
    'export type Status = "active" | "suspended";',
    '',
  ].join('\n'),
  // Two levels: the index re-exports an inner barrel that re-exports the file.
  'src/core/inner/index.ts': 'export * from "../api.js";\n',
  'src/core/index.ts': [
    'export * from "./inner/index.js";',
    'export { Account as AccountView } from "./api.js";',
    '',
  ].join('\n'),
  'src/named/index.ts': 'export { Me } from "../core/api.js";\n',
  'src/clash/a.ts': 'export type Clash = { a: number };\n',
  'src/clash/b.ts': 'export type Clash = { b: string };\n',
  'src/clash/index.ts': 'export * from "./a.js";\nexport * from "./b.js";\n',
  'src/client.ts': [
    'import type { Me } from "./core/index.js";',
    '',
    'export async function me(): Promise<Me> {',
    '  const res = await fetch("/api/v1/me");',
    '  return (await res.json()) as Me;',
    '}',
    '',
  ].join('\n'),
};

interface BundleShape {
  status: string;
  dts_content?: string;
  manifest?: Array<{ alias: string; type_string: string }>;
  symbol_failures?: Array<{ symbol_name: string; source_file: string; reason: string }>;
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#1605: the bundle follows a name through its re-exports', () => {
  let client: SidecarClient;
  let repo: string;

  before(async () => {
    repo = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1605-'));
    for (const [rel, text] of Object.entries(FILES)) {
      const abs = path.join(repo, rel);
      fs.mkdirSync(path.dirname(abs), { recursive: true });
      fs.writeFileSync(abs, text);
    }
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repo });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repo, { recursive: true, force: true });
  });

  async function bundle(
    symbol_name: string,
    source_file: string,
    extra: Record<string, unknown> = {}
  ): Promise<BundleShape> {
    return client.send<BundleShape>(
      {
        action: 'bundle',
        request_id: `${symbol_name}@${source_file}`,
        symbols: [{ symbol_name, source_file, ...extra }],
      },
      30000
    );
  }

  it('resolves a type through `export *` two levels deep', async () => {
    const res = await bundle('Me', 'src/core/index.ts', { alias: 'Me_Barrel' });
    assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
    assert.match(collapse(res.dts_content ?? ''), /export type Me_Barrel = \{ id: string; email: string; \};/);
  });

  it('resolves a type through `export { Me } from`', async () => {
    const res = await bundle('Me', 'src/named/index.ts', { alias: 'Me_Named' });
    assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
    assert.match(collapse(res.dts_content ?? ''), /export type Me_Named = \{ id: string; email: string; \};/);
  });

  it('declares a renamed re-export under the name it is exported as', async () => {
    const res = await bundle('AccountView', 'src/core/index.ts');
    assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
    const dts = collapse(res.dts_content ?? '');
    assert.match(dts, /export interface AccountView \{ id: string; plan: "basic" \| "pro"; \}/);
    assert.doesNotMatch(dts, /\bAccount\b(?!View)/);
  });

  it('wraps a re-exported union in parentheses before an array suffix', async () => {
    const res = await bundle('Status', 'src/core/index.ts', { alias: 'Statuses', array_depth: 1 });
    assert.strictEqual(res.symbol_failures, undefined, JSON.stringify(res.symbol_failures));
    assert.match(collapse(res.dts_content ?? ''), /export type Statuses = \("active" \| "suspended"\)\[\];/);
  });

  it('keeps a name two `export *` sources both provide as a failure, not a pick', async () => {
    const res = await bundle('Clash', 'src/clash/index.ts');
    assert.strictEqual(res.dts_content, undefined);
    const failure = res.symbol_failures?.find((f) => f.symbol_name === 'Clash');
    assert.ok(failure, `expected a symbol failure: ${JSON.stringify(res)}`);
    assert.match(failure.reason, /more than one/);
  });

  it('still fails a name the barrel does not export', async () => {
    const res = await bundle('Nope', 'src/core/index.ts');
    const failure = res.symbol_failures?.find((f) => f.symbol_name === 'Nope');
    assert.ok(failure, `expected a symbol failure: ${JSON.stringify(res)}`);
    assert.match(failure.reason, /not found/);
  });
});
