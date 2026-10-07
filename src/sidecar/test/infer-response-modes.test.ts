/**
 * carrick#2054: a response union whose members a request field chooses.
 *
 * A handler that answers `{ x }` for `?mode=a` and `{ y }` otherwise
 * publishes `{ x } | { y }`, and a call that states `mode=a` and reads `y`
 * is judged against the whole union. The sidecar reads, beside the published
 * union, which request field picks each member (`response_modes`): the
 * request reads the handler's branches test, and for one placed field with
 * every test read, the union each stated value receives.
 *
 * The published inference does not change: every row's `type_string` is
 * pinned to the text the inference published before this reading existed.
 *
 * Every name below is a placeholder; the shapes are the ones a handler on
 * the platform `Request`/`Response` (default lib) or on a library's request
 * object takes.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const LIBRARY_DTS = `export interface Req {
  query: Record<string, string | undefined>;
}
`;

const ROUTES_TS = `import type { Req } from "http-kit";

declare const flags: { beta: boolean };
declare function loadStore(): Promise<{ ready: boolean } | null>;
declare function load(req: Request): Promise<{ mode: string }>;

interface Envelope {
  json(): Promise<{ kind: string }>;
}

export async function queryMode(req: Request) {
  const mode = new URL(req.url).searchParams.get('mode');
  if (mode === 'a') return Response.json({ x: 1 });
  return Response.json({ y: 'z' });
}

export async function chained(req: Request) {
  const mode = new URL(req.url).searchParams.get('mode');
  if (mode === 'a') return Response.json({ x: 1 });
  if (mode !== 'b') return Response.json({ c: 1 });
  return Response.json({ y: 'z' });
}

export async function bodySwitch(req: Request) {
  const { kind } = await req.json();
  switch (kind) {
    case 'p':
    case 'q':
      return Response.json({ p: true });
    case 'r':
      return Response.json({ r: 1 });
    default:
      return Response.json({ n: 0 });
  }
}

export async function conditional(req: Request) {
  const params = new URL(req.url).searchParams;
  return params.get('mode') === 'a' ? Response.json({ x: 1 }) : Response.json({ y: 'z' });
}

export async function flipped(req: Request) {
  const mode = new URL(req.url).searchParams.get('mode');
  if (!('a' !== mode)) return Response.json({ x: 1 });
  return Response.json({ y: 'z' });
}

export async function guarded(req: Request) {
  const store = await loadStore();
  if (!store) return Response.json({ s: 'empty' });
  const mode = new URL(req.url).searchParams.get('mode');
  if (mode === 'a') return Response.json({ x: 1 });
  return Response.json({ y: 'z' });
}

export async function validated(req: Request) {
  const body = await req.json();
  if (!body.name) return Response.json({ error: 'name required' }, { status: 400 });
  if (body.kind === 'r') return Response.json({ r: 1 });
  return Response.json({ n: 0 });
}

export async function truthy(req: Request) {
  const id = new URL(req.url).searchParams.get('id');
  if (id) return Response.json({ one: 1 });
  return Response.json({ many: [1] });
}

export function nodeStyle(req: Req) {
  if (req.query.mode === 'a') return Response.json({ x: 1 });
  return Response.json({ y: 'z' });
}

export async function twoFields(req: Request) {
  const params = new URL(req.url).searchParams;
  if (params.get('mode') === 'a') return Response.json({ x: 1 });
  if (params.get('view') === 'b') return Response.json({ v: true });
  return Response.json({ y: 'z' });
}

export async function reassigned(req: Request) {
  let mode = new URL(req.url).searchParams.get('mode');
  if (!mode) mode = 'a';
  if (mode === 'a') return Response.json({ x: 1 });
  return Response.json({ y: 'z' });
}

export async function defaulted(req: Request) {
  const { kind = 'r' } = await req.json();
  if (kind === 'r') return Response.json({ r: 1 });
  return Response.json({ n: 0 });
}

export async function loaded(req: Request) {
  const { mode } = await load(req);
  if (mode === 'a') return Response.json({ x: 1 });
  return Response.json({ y: 'z' });
}

export async function ownJson(req: Request, envelope: Envelope) {
  void req;
  const { kind } = await envelope.json();
  if (kind === 'r') return Response.json({ r: 1 });
  return Response.json({ n: 0 });
}

export async function serverState(req: Request) {
  void req;
  if (flags.beta) return Response.json({ x: 1 });
  return Response.json({ y: 'z' });
}

export async function sameEitherWay(req: Request) {
  const mode = new URL(req.url).searchParams.get('mode');
  if (flags.beta) {
    if (mode === 'a') return Response.json({ x: 1 });
    return Response.json({ x: 2 });
  }
  return Response.json({ y: 'z' });
}

export async function afterSwitch(req: Request) {
  const mode = new URL(req.url).searchParams.get('mode');
  switch (mode) {
    case 'a':
      return Response.json({ x: 1 });
  }
  return Response.json({ y: 'z' });
}
`;

/**
 * The `type_string` each row published before `response_modes` existed
 * (origin/main 126e4508, read with this fixture). The reading must not move
 * a byte of it.
 */
const PUBLISHED: Record<string, string> = {
  queryMode: '{ x: number; } | { y: string; }',
  chained: '{ x: number; } | { c: number; } | { y: string; }',
  bodySwitch: '{ p: boolean; } | { r: number; } | { n: number; }',
  conditional: '{ x: number; } | { y: string; }',
  flipped: '{ x: number; } | { y: string; }',
  guarded: '{ s: string; } | { x: number; } | { y: string; }',
  validated: '{ r: number; } | { n: number; }',
  truthy: '{ one: number; } | { many: number[]; }',
  nodeStyle: '{ x: number; } | { y: string; }',
  twoFields: '{ x: number; } | { v: boolean; } | { y: string; }',
  reassigned: '{ x: number; } | { y: string; }',
  defaulted: '{ r: number; } | { n: number; }',
  loaded: '{ x: number; } | { y: string; }',
  ownJson: '{ r: number; } | { n: number; }',
  serverState: '{ x: number; } | { y: string; }',
  sameEitherWay: '{ x: number; } | { y: string; }',
  afterSwitch: '{ x: number; } | { y: string; }',
};

interface Read {
  location: string;
  field: string;
}
interface Case {
  value: string | null;
  type_string: string;
}
interface Inferred {
  alias: string;
  type_string: string;
  response_modes?: { reads: Read[]; cases?: Case[] };
}

const lineOf = (text: string): number => {
  const at = ROUTES_TS.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  assert.strictEqual(ROUTES_TS.indexOf(text, at + 1), -1, `fixture must contain exactly one: ${text}`);
  return ROUTES_TS.slice(0, at).split('\n').length;
};

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

const HANDLERS = [
  'queryMode',
  'chained',
  'bodySwitch',
  'conditional',
  'flipped',
  'guarded',
  'validated',
  'truthy',
  'nodeStyle',
  'twoFields',
  'reassigned',
  'defaulted',
  'loaded',
  'ownJson',
  'serverState',
  'sameEitherWay',
  'afterSwitch',
];

describe('carrick#2054: the request field that picks each member of a response union', () => {
  let client: SidecarClient;
  let repoDir: string;
  let inferred: Map<string, Inferred>;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2054-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    const library = path.join(repoDir, 'node_modules', 'http-kit');
    fs.mkdirSync(library, { recursive: true });
    fs.writeFileSync(
      path.join(library, 'package.json'),
      JSON.stringify({ name: 'http-kit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(library, 'index.d.ts'), LIBRARY_DTS);
    const routesPath = path.join(repoDir, 'src', 'routes.ts');
    fs.writeFileSync(routesPath, ROUTES_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-2054', repo_root: repoDir });
    const res = await client.send<{ status: string; inferred_types?: Inferred[] }>({
      action: 'infer',
      request_id: 'infer-2054',
      requests: HANDLERS.map((name) => {
        const declaration = name === 'nodeStyle' ? `export function ${name}(` : `export async function ${name}(`;
        return {
          file_path: routesPath,
          line_number: lineOf(declaration),
          infer_kind: 'response_body',
          alias: name,
        };
      }),
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    inferred = new Map((res.inferred_types ?? []).map((t) => [t.alias, t]));
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const row = (alias: string): Inferred => {
    const t = inferred.get(alias);
    assert.ok(t, `expected an inferred type for ${alias}`);
    return t;
  };
  const cases = (alias: string): Array<[string | null, string]> =>
    (row(alias).response_modes?.cases ?? []).map((c) => [c.value, collapse(c.type_string)]);

  it('publishes every union exactly as before', () => {
    const published = Object.fromEntries(HANDLERS.map((name) => [name, row(name).type_string]));
    assert.deepStrictEqual(published, PUBLISHED);
  });

  it('reads a query parameter tested before an early return', () => {
    assert.deepStrictEqual(row('queryMode').response_modes?.reads, [{ location: 'query', field: 'mode' }]);
    assert.deepStrictEqual(cases('queryMode'), [
      ['a', '{ x: number; }'],
      [null, '{ y: string; }'],
    ]);
  });

  it('narrows a field tested twice to what both tests let through', () => {
    assert.deepStrictEqual(cases('chained'), [
      ['a', '{ x: number; }'],
      ['b', '{ y: string; }'],
      [null, '{ c: number; }'],
    ]);
  });

  it('reads a body field a switch tests, with grouped labels and a default', () => {
    assert.deepStrictEqual(row('bodySwitch').response_modes?.reads, [{ location: 'body', field: 'kind' }]);
    assert.deepStrictEqual(cases('bodySwitch'), [
      ['p', '{ p: boolean; }'],
      ['q', '{ p: boolean; }'],
      ['r', '{ r: number; }'],
      [null, '{ n: number; }'],
    ]);
  });

  it('reads both arms of a conditional expression', () => {
    assert.deepStrictEqual(cases('conditional'), [
      ['a', '{ x: number; }'],
      [null, '{ y: string; }'],
    ]);
  });

  it('reads a negated inequality with the literal on the left', () => {
    assert.deepStrictEqual(cases('flipped'), [
      ['a', '{ x: number; }'],
      [null, '{ y: string; }'],
    ]);
  });

  it('keeps a body sent under no request test in every case', () => {
    assert.deepStrictEqual(row('guarded').response_modes?.reads, [{ location: 'query', field: 'mode' }]);
    assert.deepStrictEqual(cases('guarded'), [
      ['a', '{ s: string; } | { x: number; }'],
      [null, '{ s: string; } | { y: string; }'],
    ]);
  });

  it('ignores a validation guard whose branch only sends an error', () => {
    assert.deepStrictEqual(row('validated').response_modes?.reads, [{ location: 'body', field: 'kind' }]);
    assert.deepStrictEqual(cases('validated'), [
      ['r', '{ r: number; }'],
      [null, '{ n: number; }'],
    ]);
  });

  it('lists a truthiness test as a read with no cases', () => {
    assert.deepStrictEqual(row('truthy').response_modes, { reads: [{ location: 'query', field: 'id' }] });
  });

  it("leaves a library's own request accessor unplaced", () => {
    assert.deepStrictEqual(row('nodeStyle').response_modes, { reads: [{ location: 'unplaced', field: 'mode' }] });
  });

  it('lists two fields and reads no cases', () => {
    assert.deepStrictEqual(row('twoFields').response_modes, {
      reads: [
        { location: 'query', field: 'mode' },
        { location: 'query', field: 'view' },
      ],
    });
  });

  it('does not follow a reassigned binding, a binding default, or a call that takes the request', () => {
    assert.strictEqual(row('reassigned').response_modes, undefined);
    assert.strictEqual(row('defaulted').response_modes, undefined);
    assert.strictEqual(row('loaded').response_modes, undefined);
  });

  it("leaves a json() the repo declares unplaced", () => {
    assert.deepStrictEqual(row('ownJson').response_modes, { reads: [{ location: 'unplaced', field: 'kind' }] });
  });

  it('reads nothing from branches on server state', () => {
    assert.strictEqual(row('serverState').response_modes, undefined);
  });

  it('reads nothing when every case receives the same union', () => {
    assert.strictEqual(row('sameEitherWay').response_modes, undefined);
  });

  it('reads a body after a switch on the field as a test it cannot read', () => {
    assert.deepStrictEqual(row('afterSwitch').response_modes, { reads: [{ location: 'query', field: 'mode' }] });
  });
});
