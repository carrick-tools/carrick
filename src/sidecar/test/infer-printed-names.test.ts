/**
 * carrick#1836: an inference says which declaration each name it printed
 * bare means, when the request's own file cannot see that name.
 *
 * The structural printer hands some subtrees back to the compiler's print with
 * no enclosing declaration, and that print writes every named type by its bare
 * name. A database client's row type is a library's mapped type, so a field
 * typed by the client's enum (`status: EntityStatus`) prints bare. The file
 * the request names never imports that enum, so the name read there, or in the
 * capture's surface entry, means nothing (TS2304) and the member reads `any`.
 *
 * The compiler knew the symbol when it printed the name. The inference now
 * records it: `printed_names` lists, for each name its text prints that the
 * request's file does not resolve, the module that declares it and the export
 * path to it there. A name printed for two different declarations is listed
 * twice, so the reader can tell it is ambiguous.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

/** A library whose result type is a mapped type it declares itself. */
const KIT_DTS = `export declare function pick<T>(value: T): { [K in keyof T]: T[K] };
`;

const MODEL_TS = `export type Status = 'open' | 'closed';
export enum Color {
  Red = 'RED',
  Blue = 'BLUE',
}
export namespace Billing {
  export type Kind = 'card' | 'transfer';
}
/** Generated clients name types with a \$ (a regex metacharacter). */
export type $Tier = 'gold' | 'basic';
export interface Model {
  id: string;
  status: Status;
  color: Color;
  kind: Billing.Kind;
}
`;

const LEGACY_TS = `export type Status = 'a' | 'b';
export interface Legacy {
  status: Status;
}
`;

const REPO_TS = `import { pick } from 'kit';
import type { Model, $Tier } from './model';
import type { Legacy } from './legacy';

declare const row: Model;
declare const old: Legacy;
declare const tiered: { tier: $Tier };

export function load() {
  return pick(row);
}

export function loadBoth() {
  return { current: pick(row), previous: pick(old) };
}

export function loadTier() {
  return pick(tiered);
}
`;

const ROUTER_TS = `import { load, loadBoth, loadTier } from './repo';
import type { Color } from './model';

declare function send(body: unknown): void;

export function one() {
  return send(load());
}

export function both() {
  return send(loadBoth());
}

export function tier() {
  return send(loadTier());
}

export function colorOnly(color: Color) {
  return send({ color });
}
`;

const lineOf = (text: string): number => {
  const at = ROUTER_TS.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  assert.strictEqual(ROUTER_TS.indexOf(text, at + 1), -1, `fixture must contain exactly one: ${text}`);
  return ROUTER_TS.slice(0, at).split('\n').length;
};

interface PrintedName {
  name: string;
  file: string;
  export_path: string[];
}

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string; printed_names?: PrintedName[] }>;
}

describe('carrick#1836: an inference records what each bare printed name means', () => {
  let client: SidecarClient;
  let repoDir: string;
  let routerPath: string;

  before(async () => {
    // Real path: the compiler reports real paths, and the assertions read them.
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1836-infer-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['es2022'] },
        include: ['src'],
      })
    );
    const kit = path.join(repoDir, 'node_modules', 'kit');
    fs.mkdirSync(kit, { recursive: true });
    fs.writeFileSync(path.join(kit, 'package.json'), JSON.stringify({ name: 'kit', version: '1.0.0', types: 'index.d.ts' }));
    fs.writeFileSync(path.join(kit, 'index.d.ts'), KIT_DTS);
    fs.writeFileSync(path.join(repoDir, 'src', 'model.ts'), MODEL_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'legacy.ts'), LEGACY_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'repo.ts'), REPO_TS);
    routerPath = path.join(repoDir, 'src', 'router.ts');
    fs.writeFileSync(routerPath, ROUTER_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-1836', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (
    alias: string,
    expression: string
  ): Promise<{ text: string; names: PrintedName[] }> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: routerPath,
          line_number: lineOf(expression),
          expression_text: expression,
          expression_line: lineOf(expression),
          infer_kind: 'response_body',
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    assert.ok(inferred, `expected an inferred type for ${alias}, got ${JSON.stringify(response)}`);
    return { text: inferred.type_string.replace(/\s+/g, ' ').trim(), names: inferred.printed_names ?? [] };
  };

  const relative = (names: PrintedName[]) =>
    names
      .map((entry) => ({ ...entry, file: path.relative(repoDir, entry.file).split(path.sep).join('/') }))
      .sort((a, b) => `${a.name} ${a.file}`.localeCompare(`${b.name} ${b.file}`));

  it('names a type printed bare inside a library type by its declaring module', async () => {
    const { text, names } = await infer('One', 'send(load())');
    // The precondition: the library's mapped type is printed by the compiler,
    // which writes the client's own names bare.
    for (const member of ['status: Status', 'kind: Kind']) {
      assert.ok(text.includes(member), `the fixture must print \`${member}\` bare, got: ${text}`);
    }
    const listed = relative(names);
    assert.deepStrictEqual(
      listed.filter((entry) => entry.name === 'Status' || entry.name === 'Kind'),
      [
        { name: 'Kind', file: 'src/model.ts', export_path: ['Billing', 'Kind'] },
        { name: 'Status', file: 'src/model.ts', export_path: ['Status'] },
      ],
      `got ${JSON.stringify(listed)}`
    );
  });

  it('does not list a name the request file resolves itself', async () => {
    const { text, names } = await infer('One2', 'send(load())');
    assert.ok(/\bColor\b/.test(text), `the fixture must print Color, got: ${text}`);
    assert.ok(
      !names.some((entry) => entry.name === 'Color'),
      `the router imports Color, so the capture reads it there; got ${JSON.stringify(names)}`
    );
  });

  it('lists a name that carries a $', async () => {
    const { text, names } = await infer('Tier', 'send(loadTier())');
    assert.ok(text.includes('tier: $Tier'), `the fixture must print $Tier bare, got: ${text}`);
    assert.deepStrictEqual(relative(names), [{ name: '$Tier', file: 'src/model.ts', export_path: ['$Tier'] }]);
  });

  it('lists every declaration a name was printed for', async () => {
    const { text, names } = await infer('Both', 'send(loadBoth())');
    assert.ok(/previous: \{ status: Status; \}/.test(text), `the fixture must print the legacy Status bare, got: ${text}`);
    assert.deepStrictEqual(
      relative(names).filter((entry) => entry.name === 'Status'),
      [
        { name: 'Status', file: 'src/legacy.ts', export_path: ['Status'] },
        { name: 'Status', file: 'src/model.ts', export_path: ['Status'] },
      ]
    );
  });

  it('lists nothing when every name resolves where the request was made', async () => {
    const response = await client.send<{ inferred_types?: Array<Record<string, unknown>> }>({
      action: 'infer',
      request_id: 'infer-ColorOnly',
      requests: [
        {
          file_path: routerPath,
          line_number: lineOf('send({ color })'),
          expression_text: 'send({ color })',
          expression_line: lineOf('send({ color })'),
          infer_kind: 'response_body',
          alias: 'ColorOnly',
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === 'ColorOnly');
    assert.ok(inferred, JSON.stringify(response));
    assert.ok(!('printed_names' in inferred), `nothing to list, got ${JSON.stringify(inferred)}`);
  });
});
