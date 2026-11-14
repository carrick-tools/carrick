/**
 * carrick#2148 H3: a request contract read off a schema value names where that
 * schema is declared.
 *
 * A route that validates its body with a schema states its contract in the
 * schema, not in a type: the declared contract's text is the schema's input.
 * Where that schema is one named `const` in the repo, the answer's
 * `type_home` is the const's declaration, marked `kind: "schema"` so a reader
 * never prints it as a type. A schema written inline at its use has no name
 * to open, and gets no home.
 *
 * One in-test project, three routes:
 *  - a validator middleware bound to the body, handed an imported schema;
 *  - a handler that hands the body it reads to `Schema.safeParse`;
 *  - a validator middleware handed a schema built inline.
 *
 * The schema library is a stub under `node_modules`; no library name is
 * matched, only the schema types it declares.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const CHECKS_DTS = `export interface Check<Output, Input = Output> {
  parse(data: unknown): Output;
  safeParse(data: unknown): { success: true; data: Output } | { success: false };
  readonly _output: Output;
  readonly _input: Input;
}
type Shape = Record<string, Check<unknown>>;
export declare function record<S extends Shape>(
  shape: S
): Check<{ [K in keyof S]: S[K]['_output'] }, { [K in keyof S]: S[K]['_input'] }>;
export declare function text(): Check<string>;
export declare function count(): Check<number>;
`;

const SCHEMAS_TS = `import { record, text, count } from 'checks';

/** What a caller sends to open a ticket. */
export const NewTicket = record({
  title: text(),
  priority: count(),
});
`;

const ROUTER_TS = `export interface Ctx {
  body(): Promise<unknown>;
  reply(value: unknown): unknown;
}
type Handler = (ctx: Ctx) => unknown;
export interface Router {
  post(path: string, ...handlers: Handler[]): void;
}
export declare const router: Router;
export declare function guard(part: string, schema: unknown): Handler;
`;

const MIDDLEWARE_TS = `import { router, guard } from './router';
import { NewTicket } from './schemas';

router.post('/tickets', guard('json', NewTicket), async (ctx) => {
  return ctx.reply({ ok: true });
});
`;

const HANDLER_TS = `import { NewTicket } from './schemas';

export async function POST(request: Request) {
  const parsed = NewTicket.safeParse(await request.json());
  return Response.json({ ok: parsed.success });
}
`;

const INLINE_TS = `import { record, text } from 'checks';
import { router, guard } from './router';

router.post('/notes', guard('json', record({ body: text() })), async (ctx) => {
  return ctx.reply({ ok: true });
});
`;

interface Home {
  symbol: string;
  file_path: string;
  line: number;
  kind?: string;
}

interface Inferred {
  alias: string;
  type_string: string;
  primary_type_symbol?: string;
  type_home?: Home;
}

describe('a_validated_body_names_its_schema_declaration', () => {
  let client: SidecarClient;
  let repoDir: string;
  const sources: Record<string, string> = {
    'middleware.ts': MIDDLEWARE_TS,
    'handler.ts': HANDLER_TS,
    'inline.ts': INLINE_TS,
  };

  before(async () => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2148-h3-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    const pkgDir = path.join(repoDir, 'node_modules', 'checks');
    fs.mkdirSync(pkgDir, { recursive: true });
    fs.writeFileSync(
      path.join(pkgDir, 'package.json'),
      JSON.stringify({ name: 'checks', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(path.join(pkgDir, 'index.d.ts'), CHECKS_DTS);
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
    fs.writeFileSync(path.join(repoDir, 'src', 'schemas.ts'), SCHEMAS_TS);
    fs.writeFileSync(path.join(repoDir, 'src', 'router.ts'), ROUTER_TS);
    for (const [file, source] of Object.entries(sources)) {
      fs.writeFileSync(path.join(repoDir, 'src', file), source);
    }

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The 1-based line of the one line of `file` containing `marker`. */
  function lineOf(file: string, marker: string): number {
    const lines = sources[file].split('\n');
    const hits = lines.flatMap((line, index) => (line.includes(marker) ? [index + 1] : []));
    assert.strictEqual(hits.length, 1, `${marker} must name one line of ${file}`);
    return hits[0];
  }

  /** The registration that starts at `marker`, asked by its span. */
  async function registration(file: string, marker: string, alias: string): Promise<Inferred> {
    const source = sources[file];
    const start = source.indexOf(marker);
    assert.ok(start >= 0, `${file} must contain ${marker}`);
    const end = source.indexOf('\n});', start) + '\n})'.length;
    return infer({
      file_path: path.join(repoDir, 'src', file),
      line_number: lineOf(file, marker),
      infer_kind: 'request_body',
      alias,
      span_start: start,
      span_end: end,
    });
  }

  async function infer(request: Record<string, unknown>): Promise<Inferred> {
    const alias = String(request.alias);
    const res = await client.send<{ inferred_types?: Inferred[] }>({
      action: 'infer',
      request_id: alias,
      requests: [request],
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, `${alias} must be answered`);
    return inferred;
  }

  const newTicketHome = (): Home => ({
    symbol: 'NewTicket',
    file_path: path.join(repoDir, 'src', 'schemas.ts'),
    line: 4,
    kind: 'schema',
  });

  it('a validator middleware handed an imported schema: the home is the schema const', async () => {
    const inferred = await registration('middleware.ts', "router.post('/tickets'", 'Middleware');
    assert.match(inferred.type_string, /title: string/, JSON.stringify(inferred));
    assert.strictEqual(inferred.primary_type_symbol, undefined, 'the symbol is untouched');
    assert.deepStrictEqual(inferred.type_home, newTicketHome());
  });

  it('a body handed to Schema.safeParse: the home is the schema const', async () => {
    const line = lineOf('handler.ts', 'NewTicket.safeParse(');
    const inferred = await infer({
      file_path: path.join(repoDir, 'src', 'handler.ts'),
      line_number: line,
      infer_kind: 'request_body',
      alias: 'SafeParse',
      expression_text: 'await request.json()',
      expression_line: line,
    });
    assert.match(inferred.type_string, /title: string/, JSON.stringify(inferred));
    assert.strictEqual(inferred.primary_type_symbol, undefined, 'the symbol is untouched');
    assert.deepStrictEqual(inferred.type_home, newTicketHome());
  });

  it('a schema written inline has no home', async () => {
    const inferred = await registration('inline.ts', "router.post('/notes'", 'Inline');
    assert.match(inferred.type_string, /body: string/, JSON.stringify(inferred));
    assert.strictEqual(inferred.type_home, undefined, JSON.stringify(inferred.type_home));
  });
});
