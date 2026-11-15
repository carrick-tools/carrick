/**
 * carrick#937: the retype's procedure form. A procedure call's type is the
 * router's own (`client.orders.list.query()` types as the router's output),
 * so comparing it with the producer's published output compares one type
 * with itself and always clears. What breaks is the consumer's USE of it:
 * the consumer's file is type-checked as it is, and again with only the
 * call's result erased to `any`. Diagnostics present as-is and gone when
 * erased are the places the consumer reads what the procedure does not
 * return. A guard first checks that the call's result carries the
 * producer's published output (as declared or in its JSON wire form), so a
 * call whose result is something else (an options builder) is never read as
 * agreeing.
 *
 * The procedure kit is the PR seeds' own synthetic kit, copied byte for byte
 * from `tests/fixtures/pr-seeds/rpc-api`; the routers and views are written
 * here.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// dist/test -> dist -> sidecar -> src -> repo root
const KIT_SOURCE = path.join(
  __dirname,
  '..',
  '..',
  '..',
  '..',
  'tests',
  'fixtures',
  'pr-seeds',
  'rpc-api',
  'node_modules',
  'fixture-procs',
  'index.d.ts'
);

/**
 * The same kit with a client that maps each output through a JSON
 * serialiser, as a transformer-less client over the wire would type it.
 */
function jsonKit(kit: string): string {
  const mapped = kit.replace(
    '? { query(input?: unknown): Promise<O> }',
    '? { query(input?: unknown): Promise<Json<O>> }'
  );
  assert.notStrictEqual(mapped, kit, 'the kit no longer has the query member this test maps');
  return (
    mapped +
    '\ntype Json<T> = T extends Date ? string : T extends object ? { [K in keyof T]: Json<T[K]> } : T;\n'
  );
}

const KIT = `import { init } from "fixture-procs";

export const t = init.create();
`;

/** A router whose `orders.list` returns `row` (an object type's members). */
function router(row: string, resolver = '(): OrderSummary[] => placed', value = '[]'): string {
  return `import { t } from "./kit";

export type OrderSummary = ${row};

const placed: OrderSummary[] = ${value};

export const appRouter = t.router({
  ping: t.procedure.query(() => "pong"),
  orders: t.router({
    list: t.procedure.query(${resolver}),
  }),
});

export type AppRouter = typeof appRouter;
`;
}

const MAIN_ROUTER = router('{ id: string; total: number }', undefined, '[{ id: "1", total: 1200 }]');
const HEAD_ROUTER = router('{ id: string }', undefined, '[{ id: "1" }]');
const ANY_ROUTER = router('{ id: string; total: number }', '(): any => placed', '[{ id: "1", total: 1200 }]');

const WIRE_KIT = `import { init } from "fixture-procs-json";

export const t = init.create();
`;
const WIRE_ROUTER = `import { t } from "./wire-kit";

export const wireRouter = t.router({
  stamp: t.router({
    get: t.procedure.query(() => ({ at: new Date() })),
  }),
});

export type WireRouter = typeof wireRouter;
`;

/** The seed's view (`src/web/orders-view.ts`) against the router `from`. */
function vanillaView(from: string): string {
  return `import { createClient } from "fixture-procs";
import type { AppRouter } from "../${from}";

const client = createClient<AppRouter>({ url: "/api/trpc" });

export async function totals(): Promise<number[]> {
  const orders = await client.orders.list.query();
  return orders.map((order) => order.total);
}
`;
}

/**
 * A hook read with no check on the envelope's own `undefined`: the file has
 * a TS18048 on `result.data` whatever the procedure returns. That error is
 * the client's, not the output's, so it must not read as a break.
 */
function envelopeView(from: string): string {
  return `import { createHooks } from "fixture-procs";
import type { AppRouter } from "../${from}";

const hooks = createHooks<AppRouter>();

export function firstTotal(): number {
  const result = hooks.orders.list.useQuery();
  return result.data[0].total;
}
`;
}

const VIEWS: Record<string, string> = {
  'head-view.ts': vanillaView('router-head'),
  'main-view.ts': vanillaView('router-main'),
  'any-view.ts': vanillaView('router-any'),
  'hook-view.ts': `import { createHooks } from "fixture-procs";
import type { AppRouter } from "../router-head";

const hooks = createHooks<AppRouter>();

export function firstTotal(): number | undefined {
  const first = hooks.orders.list.useQuery().data?.[0];
  return first?.total;
}
`,
  'envelope-main-view.ts': envelopeView('router-main'),
  'envelope-head-view.ts': envelopeView('router-head'),
  'options-view.ts': `import { createContext } from "fixture-procs";
import type { AppRouter } from "../router-main";

const unused = 0;

export function key(): string[] {
  const options = createContext<AppRouter>().useProxy().orders.list.queryOptions();
  return options.key;
}
`,
  'twice-view.ts': `import { createClient } from "fixture-procs";
import type { AppRouter } from "../router-main";

const client = createClient<AppRouter>({ url: "/api/trpc" });

export async function both(): Promise<number> {
  const [a, b] = await Promise.all([client.orders.list.query(), client.orders.list.query()]);
  return a.length + b.length;
}
`,
  'discard-view.ts': `import { createClient } from "fixture-procs";
import type { AppRouter } from "../router-main";

const client = createClient<AppRouter>({ url: "/api/trpc" });

export async function warm(): Promise<void> {
  await client.orders.list.query();
  return;
}
`,
  'wire-view.ts': `import { createClient } from "fixture-procs-json";
import type { WireRouter } from "../wire-router";

const client = createClient<WireRouter>({ url: "/api/trpc" });

export async function stampLength(): Promise<number> {
  const one = await client.stamp.get.query();
  return one.at.length;
}
`,
};

/** Every view puts its procedure call on line 7. */
const CALL_LINE = 7;

interface Outcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'abstain' | 'wider';
  diagnostics: Array<{ line: number; code: number; message: string }>;
  reason?: string;
}

describe('carrick#937: the retype judges a procedure call by the consumer\'s own code', () => {
  let client: SidecarClient;
  let repoDir: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-937-'));
    const kit = fs.readFileSync(KIT_SOURCE, 'utf8');
    const write = (rel: string, text: string) => {
      const file = path.join(repoDir, rel);
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, text);
    };
    write(
      'tsconfig.json',
      JSON.stringify({
        compilerOptions: {
          target: 'es2022',
          module: 'commonjs',
          moduleResolution: 'node',
          strict: true,
          skipLibCheck: true,
          types: [],
        },
        include: ['src/**/*'],
      })
    );
    write('node_modules/fixture-procs/package.json', '{ "name": "fixture-procs", "version": "1.0.0", "types": "index.d.ts" }\n');
    write('node_modules/fixture-procs/index.d.ts', kit);
    write(
      'node_modules/fixture-procs-json/package.json',
      '{ "name": "fixture-procs-json", "version": "1.0.0", "types": "index.d.ts" }\n'
    );
    write('node_modules/fixture-procs-json/index.d.ts', jsonKit(kit));
    write('src/kit.ts', KIT);
    write('src/router-main.ts', MAIN_ROUTER);
    write('src/router-head.ts', HEAD_ROUTER);
    write('src/router-any.ts', ANY_ROUTER);
    write('src/wire-kit.ts', WIRE_KIT);
    write('src/wire-router.ts', WIRE_ROUTER);
    for (const [name, text] of Object.entries(VIEWS)) write(`src/web/${name}`, text);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The item as the scanner sends a procedure pair: file, line, key path. */
  async function judge(
    view: string,
    producerType: string,
    procedurePath = 'orders.list'
  ): Promise<Outcome> {
    const res = await client.send<{ status: string; outcomes?: Outcome[] }>({
      action: 'retype_check',
      request_id: `${view}-${producerType}`,
      items: [
        {
          item_id: view,
          file_path: path.join(repoDir, 'src', 'web', view),
          line_number: CALL_LINE,
          producer_type: producerType,
          wire: true,
          procedure_path: procedurePath,
        },
      ],
    });
    assert.strictEqual(res.status, 'success', JSON.stringify(res));
    assert.strictEqual(res.outcomes?.length, 1);
    return res.outcomes![0];
  }

  it('finds the read of a field the procedure no longer returns', async () => {
    const out = await judge('head-view.ts', '{ id: string; }[]');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.deepStrictEqual(
      out.diagnostics.map((d) => ({ line: d.line, code: d.code })),
      [{ line: 8, code: 2339 }]
    );
    assert.match(out.diagnostics[0].message, /Property 'total' does not exist/);
  });

  it('agrees when every field the consumer reads is returned', async () => {
    const out = await judge('main-view.ts', '{ id: string; total: number; }[]');
    assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
    assert.deepStrictEqual(out.diagnostics, []);
  });

  it('reads through a hook envelope to the read that breaks', async () => {
    const out = await judge('hook-view.ts', '{ id: string; }[]');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.deepStrictEqual(
      out.diagnostics.map((d) => ({ line: d.line, code: d.code })),
      [{ line: 8, code: 2339 }]
    );
  });

  it('does not count an error in the client\'s envelope as a break', async () => {
    const out = await judge('envelope-main-view.ts', '{ id: string; total: number; }[]');
    assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
  });

  it('still finds the read of a removed field beside an error in the envelope', async () => {
    const out = await judge('envelope-head-view.ts', '{ id: string; }[]');
    assert.strictEqual(out.outcome, 'mismatch', JSON.stringify(out));
    assert.deepStrictEqual(
      out.diagnostics.map((d) => ({ line: d.line, code: d.code })),
      [{ line: 8, code: 2339 }]
    );
  });

  it('does not compare a call whose result is not the procedure output (an options builder)', async () => {
    const out = await judge('options-view.ts', '{ id: string; total: number; }[]');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /does not carry the procedure's output/);
  });

  it('does not compare against a published output the call does not carry (stale)', async () => {
    const out = await judge('head-view.ts', '{ id: string; total: number; }[]');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /does not carry the procedure's output/);
  });

  it('abstains when two calls to the procedure start on one line', async () => {
    const out = await judge('twice-view.ts', '{ id: string; total: number; }[]');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /two calls to the procedure/);
  });

  it('abstains when the consumer never reads the result', async () => {
    const out = await judge('discard-view.ts', '{ id: string; total: number; }[]');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /never reads the response/);
  });

  it('does not compare a call whose result reads as any', async () => {
    const out = await judge('any-view.ts', '{ id: string; total: number; }[]');
    assert.strictEqual(out.outcome, 'abstain', JSON.stringify(out));
    assert.match(out.reason ?? '', /reads as 'any'/);
  });

  it('carries an output the client types in its JSON wire form', async () => {
    const out = await judge('wire-view.ts', '{ at: Date; }', 'stamp.get');
    assert.strictEqual(out.outcome, 'agrees', JSON.stringify(out));
  });
});

describe('carrick#937: retype_check keeps the procedure path', () => {
  it('parses an item with procedure_path and keeps it', async () => {
    const { parseRequest } = await import('../src/validators.js');
    const result = parseRequest({
      action: 'retype_check',
      request_id: 'p-1',
      items: [
        {
          item_id: 'i',
          file_path: 'src/web/view.ts',
          line_number: 7,
          producer_type: '{ id: string; }[]',
          wire: true,
          procedure_path: 'orders.list',
        },
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result));
    const items = (result as unknown as { request: { items: Array<Record<string, unknown>> } }).request
      .items;
    assert.strictEqual(items[0].procedure_path, 'orders.list');
  });
});
