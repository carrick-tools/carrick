/**
 * carrick#4244 T3a: an answer names the nodes its type text was built from.
 *
 * `source_location` names one node, and on a producer that is the handler,
 * so a reader cannot tell which sends or reads made the published text.
 * `read_spans` lists those nodes as `{ file_path, start, end }`, in the
 * compiler's own positions (UTF-16 code units from 0, end exclusive), and only
 * those: an error send the reader dropped, or a second body access it did not
 * read, is not listed. A decided abstain publishes no text, so it lists
 * nothing.
 *
 * `res-kit` is a synthetic stand-in for a server library: no framework name
 * is read.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const KIT_DTS = `export interface Res {
  statusCode: number;
  setHeader(name: string, value: string): Res;
  getHeader(name: string): string | undefined;
  status(code: number): Res;
  json(body: unknown): Res;
  send(body?: unknown): Res;
  set(fields: Record<string, string>): Res;
}
export interface Req {
  body: any;
  json(): Promise<unknown>;
}
export type Handler = (req: Req, res: Res) => unknown;
export declare class App {
  get(path: string, handler: Handler): App;
  get(path: string, options: { schema?: unknown }, handler: Handler): App;
  post(path: string, handler: Handler): App;
}
`;

const ROUTES_TS = `import { App, type Req } from "res-kit";
import { ping } from "./handlers";

interface Item { id: string; label: string }
interface NewWidget { name: string; size: number }
interface TypedReq<B> extends Req { body: B }

declare function save(body: unknown): Promise<{ ok: boolean; item: Item }>;
declare function load(): Promise<Item>;
declare const ItemSchema: { parse(value: unknown): Item };

const app = new App();

app.get('/two-success', async (req, res) => {
  const result = await save(req.body);
  if (!result.ok) {
    res.status(400).json({ bad: true });
    return;
  }
  if (result.item.label) {
    res.status(201).json({ made: result.item.label });
    return;
  }
  res.json({ none: true });
});

app.get('/literal', async () => ({ ok: true, at: 1 }));

app.get('/annotated', async (): Promise<Item> => load());

app.get('/imported', ping);

app.post('/read', async (req, res) => {
  const body = (await req.json()) as NewWidget;
  const again = req.body.name;
  res.json({ name: body.name, again });
});

app.post('/typed-param', async (req: TypedReq<NewWidget>, res) => {
  res.json({ made: req.body.name });
});

app.get('/schema', { schema: { response: { 200: ItemSchema } } }, async () => load());

app.get('/only-error', async (req, res) => {
  res.status(404).json({ missing: true });
});

app.get('/unicode', async () => {
  // naïve café ✓ 😀: non-ASCII text before the value
  return { label: 'größe' };
});
`;

const HANDLERS_TS = `export async function ping() {
  return { pong: true, count: 2 };
}
`;

const RESOLVERS_TS = `export const resolvers = {
  Query: {
    item: async () => ({ id: '1', title: 'first' }),
  },
};
`;

const LOOP_TS = `interface Alpha { alphaId: number }
interface Beta { betaName: string }
interface LoopApp { get(path: string, handler: () => Promise<unknown>): void }
declare const app: LoopApp;
declare function rows(): { alpha: Alpha[]; beta: Beta[] };

for (const [path, key] of [['alpha-rows', 'alpha'], ['beta-rows', 'beta']] as const) app.get(\`/items/\${path}\`, async () => ({ items: rows()[key], total: 1 }));
export {};
`;

interface ReadSpan {
  file_path: string;
  start: number;
  end: number;
}

interface Answer {
  alias: string;
  type_string: string;
  source_location: { file_path: string; start_line: number };
  read_spans?: ReadSpan[];
  any_provenance?: Array<{ path: string; reason: string }>;
}

interface InferShape {
  status: string;
  inferred_types?: Answer[];
}

function lineIn(source: string, needle: string, from = 0): number {
  const at = source.indexOf(needle, from);
  assert.ok(at >= 0, `fixture must contain: ${needle}`);
  return source.slice(0, at).split('\n').length;
}

describe('carrick#4244 T3a: an answer names each value it read', () => {
  let repoDir: string;
  const files: Record<string, string> = {};
  const sources: Record<string, string> = {
    'routes.ts': ROUTES_TS,
    'handlers.ts': HANDLERS_TS,
    'resolvers.ts': RESOLVERS_TS,
    'loop.ts': LOOP_TS,
  };
  let client: SidecarClient;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4244-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.mkdirSync(path.join(repoDir, 'node_modules', 'res-kit'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'node_modules', 'res-kit', 'package.json'),
      JSON.stringify({ name: 'res-kit', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(repoDir, 'node_modules', 'res-kit', 'index.d.ts'), KIT_DTS);
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
    for (const [name, text] of Object.entries(sources)) {
      files[name] = path.join(repoDir, 'src', name);
      fs.writeFileSync(files[name], text);
    }
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  async function ask(request: Record<string, unknown>): Promise<Answer | undefined> {
    const alias = String(request.alias);
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [request],
    });
    return (res.inferred_types ?? []).find((t) => t.alias === alias);
  }

  /** A line-only ask on the registration of `route` in routes.ts. */
  function atRoute(alias: string, route: string, inferKind: string): Record<string, unknown> {
    return {
      file_path: files['routes.ts'],
      line_number: lineIn(ROUTES_TS, `('${route}'`),
      infer_kind: inferKind,
      alias,
    };
  }

  /** The source text each span covers, in the file it names. */
  function readTexts(answer: Answer | undefined): string[] {
    assert.ok(answer, 'the ask is answered');
    assert.ok(answer.read_spans, `the answer names what it read: ${JSON.stringify(answer)}`);
    return answer.read_spans.map((span) => {
      const name = path.basename(span.file_path);
      assert.ok(sources[name] !== undefined, `the span names a fixture file: ${span.file_path}`);
      return sources[name].slice(span.start, span.end);
    });
  }

  it('a_response_answer_names_each_value_it_read: two 2xx sends, not the error send', async () => {
    const got = await ask({
      ...atRoute('TwoSuccess', '/two-success', 'response_body'),
      expression_text: '{ bad: true }',
      expression_line: lineIn(ROUTES_TS, '{ bad: true }'),
    });
    assert.deepStrictEqual(readTexts(got).sort(), ['{ made: result.item.label }', '{ none: true }']);
  });

  it('names the object literal an arrow returns', async () => {
    const got = await ask(atRoute('Literal', '/literal', 'response_body'));
    assert.deepStrictEqual(readTexts(got), ['{ ok: true, at: 1 }']);
  });

  it('names the return annotation of an annotated handler', async () => {
    const got = await ask(atRoute('Annotated', '/annotated', 'response_body'));
    assert.deepStrictEqual(readTexts(got), ['Promise<Item>']);
  });

  it('names the value in the file an imported handler is declared in', async () => {
    const got = await ask(atRoute('Imported', '/imported', 'response_body'));
    assert.deepStrictEqual(readTexts(got), ['{ pong: true, count: 2 }']);
    assert.ok(got?.read_spans?.every((span) => span.file_path.endsWith('/handlers.ts')));
  });

  it('names the one body read a request was built from, not a second access', async () => {
    const got = await ask(atRoute('Read', '/read', 'request_body'));
    // The typed read the request text was taken from; `req.body.name` is
    // read too, but nothing was built from it.
    assert.deepStrictEqual(readTexts(got), ['(await req.json()) as NewWidget']);
  });

  it("names an annotated body parameter's annotation", async () => {
    const got = await ask(atRoute('TypedParam', '/typed-param', 'request_body'));
    assert.deepStrictEqual(readTexts(got), ['TypedReq<NewWidget>']);
  });

  it('names the schema value a declared response comes from', async () => {
    const got = await ask(atRoute('Schema', '/schema', 'response_body'));
    assert.deepStrictEqual(readTexts(got), ['ItemSchema']);
  });

  it("names a GraphQL resolver's returned expression", async () => {
    const start = RESOLVERS_TS.indexOf('async () =>');
    const end = RESOLVERS_TS.indexOf(',\n  },');
    const got = await ask({
      file_path: files['resolvers.ts'],
      line_number: lineIn(RESOLVERS_TS, 'async () =>'),
      span_start: start,
      span_end: end,
      infer_kind: 'function_return',
      alias: 'Resolver',
    });
    assert.deepStrictEqual(readTexts(got), ["{ id: '1', title: 'first' }"]);
  });

  it('a decided abstain names nothing', async () => {
    const got = await ask({
      ...atRoute('OnlyError', '/only-error', 'response_body'),
      expression_text: '{ missing: true }',
      expression_line: lineIn(ROUTES_TS, '{ missing: true }'),
    });
    assert.ok(got, 'the route answers with a decided abstain');
    assert.ok(
      (got.any_provenance ?? []).some((p) => p.reason === 'no_success_payload'),
      `the abstain is decided: ${JSON.stringify(got)}`
    );
    assert.strictEqual(got.read_spans, undefined);
  });

  it('counts positions in UTF-16 code units past non-ASCII text', async () => {
    const got = await ask(atRoute('Unicode', '/unicode', 'response_body'));
    assert.deepStrictEqual(readTexts(got), ["{ label: 'größe' }"]);
  });

  it('maps a loop row back to the file as written', async () => {
    const got = await ask({
      file_path: files['loop.ts'],
      line_number: lineIn(LOOP_TS, 'for (const'),
      infer_kind: 'response_body',
      alias: 'LoopBeta',
      route_path: '/items/beta-rows',
    });
    assert.match(got?.type_string ?? '', /betaName/);
    assert.deepStrictEqual(readTexts(got), ['{ items: rows()[key], total: 1 }']);
  });
});
